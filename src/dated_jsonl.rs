//! Private, append-only, one-file-per-UTC-day JSONL directories under the
//! config directory, pruned to a fixed number of days and held to a total
//! size, for the consumers of [`crate::run_record`] that keep records on
//! disk. The only files this deletes or replaces are ones named exactly
//! `YYYY-MM-DD.jsonl` inside the directory it was handed, and the scratch
//! file a trim writes beside one.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::auth;

/// `<config dir>/<name>`, created `0700`, or `None`.
///
/// A missing config directory is created `0700`; an existing one is left
/// as it is, since a record may come from a read-only command and hardening
/// it is `auth`'s job. A config path that isn't a directory is `auth`'s to
/// report.
pub(crate) fn private_dir(name: &str) -> Option<PathBuf> {
    let config = auth::config_dir_path()?;
    if config.exists() && !config.is_dir() {
        return None;
    }
    let dir = config.join(name);
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(&dir).ok()?;
        // `mode` is filtered by the umask and skipped for a directory that
        // already existed.
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    builder.create(&dir).ok()?;
    Some(dir)
}

/// Appends `line` to today's file in `dir`, and on the first write of a day
/// deletes files older than `keep_days` (today included). Best-effort.
pub(crate) fn append(dir: &Path, line: &str, keep_days: u64) {
    let now = now_secs();
    let (today, _) = utc_date(now);
    let path = dir.join(format!("{today}.jsonl"));
    let is_new_day = !path.exists();
    // One `write` per line. `O_APPEND` places each one at the end, but a
    // line past the platform's atomic-write size is not guaranteed to stay
    // whole against a parallel run writing at the same moment.
    if let Ok(mut file) = open_private(&path) {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
    if is_new_day {
        prune(dir, now, keep_days);
    }
}

/// Holds `dir`'s dated files, together, to `limit` bytes by dropping the
/// oldest lines first — whole days while a day is all that has to go, then
/// the oldest lines of the oldest day left. Sheds down to nine tenths of
/// `limit`, so the next run does not have to shed again.
pub(crate) fn shed(dir: &Path, limit: u64) {
    let mut files: Vec<(String, u64)> = dated_names(dir)
        .into_iter()
        .filter_map(|name| Some((name.clone(), std::fs::metadata(dir.join(&name)).ok()?.len())))
        .collect();
    let total: u64 = files.iter().map(|(_, size)| size).sum();
    if total <= limit {
        return;
    }
    let mut excess = total - limit / 10 * 9;
    files.sort();
    for (name, size) in files {
        if excess == 0 {
            break;
        }
        let path = dir.join(&name);
        if size <= excess {
            let _ = std::fs::remove_file(&path);
            excess -= size;
        } else {
            trim(&path, size - excess);
            excess = 0;
        }
    }
}

/// Deletes the dated files in `dir` whose date `keep` refuses.
pub(crate) fn prune_where(dir: &Path, keep: impl Fn(&str) -> bool) {
    for name in dated_names(dir) {
        if let Some(date) = dated_file(&name) {
            if !keep(date) {
                let _ = std::fs::remove_file(dir.join(&name));
            }
        }
    }
}

/// The lines of `dir`'s file for `date` (`YYYY-MM-DD`), oldest first.
pub(crate) fn read_day(dir: &Path, date: &str) -> Vec<String> {
    let path = dir.join(format!("{date}.jsonl"));
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The day after `date` (`YYYY-MM-DD`), or `None` when it isn't one.
pub(crate) fn next_date(date: &str) -> Option<String> {
    dated_file(&format!("{date}.jsonl"))?;
    let y = date.get(0..4)?.parse().ok()?;
    let m = date.get(5..7)?.parse().ok()?;
    let d = date.get(8..10)?.parse().ok()?;
    let days = crate::account_usage::days_from_civil(y, m, d);
    Some(utc_date((days as u64 + 1) * 86_400).0)
}

/// The oldest date kept by a window of `days` (today included), as of now.
pub(crate) fn oldest_kept(days: u64) -> String {
    utc_date(now_secs().saturating_sub(days.saturating_sub(1) * 86_400)).0
}

fn dated_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| dated_file(name).is_some())
        .collect()
}

/// Every line in `dir`'s dated files, oldest first. A missing directory is
/// no lines.
pub(crate) fn read_all(dir: &Path) -> Vec<String> {
    let mut names = dated_names(dir);
    names.sort();
    names
        .iter()
        .filter_map(|name| std::fs::read_to_string(dir.join(name)).ok())
        .flat_map(|text| {
            text.lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Opens `path` for appending, created `0600` if missing.
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Keeps the newest whole lines of `path` that fit in `limit` bytes.
///
/// Written to a scratch file and renamed over the original. A parallel run
/// that appends between the read and the rename loses its line: these files
/// are best-effort, and a lock would make every run pay for a rare race.
fn trim(path: &Path, limit: u64) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let kept = newest_lines(&text, limit as usize);
    if kept.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    let Some(name) = path.file_name() else {
        return;
    };
    let scratch = path.with_file_name(format!(
        ".{}.trim-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let written = create_private(&scratch)
        .and_then(|mut file| file.write_all(kept.as_bytes()))
        .and_then(|()| std::fs::rename(&scratch, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&scratch);
    }
}

/// The longest suffix of `text` made of whole lines and no longer than
/// `target` bytes.
fn newest_lines(text: &str, target: usize) -> &str {
    if text.len() <= target {
        return text;
    }
    let from = text.len() - target;
    let bytes = text.as_bytes();
    // The first line that starts at or after `from`. Always just past a
    // `\n`, so never inside a character.
    let start = if bytes[from - 1] == b'\n' {
        from
    } else {
        bytes[from..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(text.len(), |p| from + p + 1)
    };
    &text[start..]
}

/// Creates `path` `0600`, failing if it exists.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn prune(dir: &Path, now: u64, keep_days: u64) {
    let (oldest_kept, _) = utc_date(now.saturating_sub(keep_days.saturating_sub(1) * 86_400));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(date) = dated_file(&name) {
            if date < oldest_kept.as_str() {
                let _ = std::fs::remove_file(dir.join(&name));
            }
        }
    }
}

fn dated_file(name: &str) -> Option<&str> {
    let date = name.strip_suffix(".jsonl")?;
    let shape = date.len() == 10
        && date.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    shape.then_some(date)
}

/// `YYYY-MM-DD` and the seconds into that day, in UTC.
fn utc_date(unix_secs: u64) -> (String, u64) {
    let days = (unix_secs / 86_400) as i64;
    let (y, m, d) = crate::account_usage::civil_from_days(days);
    (format!("{y:04}-{m:02}-{d:02}"), unix_secs % 86_400)
}

/// RFC 3339 in UTC, to the millisecond.
pub(crate) fn timestamp(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (date, secs) = utc_date(since.as_secs());
    format!(
        "{date}T{:02}:{:02}:{:02}.{:03}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60,
        since.subsec_millis()
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dated_files_are_pruned() {
        assert_eq!(dated_file("2026-09-24.jsonl"), Some("2026-09-24"));
        for name in [
            "user-id",
            "last-version",
            "2026-09-24.json",
            "notes.jsonl",
            "2026-9-24.jsonl",
        ] {
            assert_eq!(dated_file(name), None, "{name}");
        }
    }

    #[test]
    fn prune_keeps_seven_days_and_nothing_else_is_touched() {
        let dir =
            std::env::temp_dir().join(format!("mapbox-dated-prune-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "2026-09-17.jsonl",
            "2026-09-18.jsonl",
            "2026-09-24.jsonl",
            "user-id",
            "2026-09-01.txt",
        ] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        // 2026-09-24T12:00:00Z: 09-18 through 09-24 is seven days.
        prune(&dir, 1_790_251_200, 7);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            left,
            [
                "2026-09-01.txt",
                "2026-09-18.jsonl",
                "2026-09-24.jsonl",
                "user-id"
            ]
        );
    }

    #[test]
    fn a_trim_keeps_the_newest_whole_lines() {
        let text = "aaaa\nbbbb\ncccc\n";
        assert_eq!(newest_lines(text, 100), text);
        assert_eq!(newest_lines(text, 10), "bbbb\ncccc\n");
        assert_eq!(newest_lines(text, 9), "cccc\n");
        assert_eq!(newest_lines(text, 4), "");
    }

    #[test]
    fn shedding_drops_the_oldest_days_then_the_oldest_lines() {
        let dir = std::env::temp_dir().join(format!("mapbox-dated-shed-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let day = |n: u32| format!("2026-09-{n:02}.jsonl");
        // Three days of ten 100-byte lines each.
        for n in 1..=3 {
            let text: String = (0..10).map(|i| format!("{n}-{i:<96}\n")).collect();
            std::fs::write(dir.join(day(n)), text).unwrap();
        }
        let mine = dir.join("notes.txt");
        std::fs::write(&mine, "x".repeat(5000)).unwrap();

        shed(&dir, 2000);
        let left = read_all(&dir);
        let total: usize = left.iter().map(|l| l.len() + 1).sum();
        let mine_kept = mine.exists();
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(mine_kept, "only dated files are shed");
        assert!(total <= 1800, "{total}");
        assert!(
            left.iter().all(|l| !l.starts_with("1-")),
            "the oldest day went first"
        );
        assert!(
            left.iter().any(|l| l.starts_with("2-")),
            "only as much as needed"
        );
        assert!(
            left.last().unwrap().starts_with("3-9"),
            "the newest line stays"
        );
    }

    #[test]
    fn the_next_date_crosses_months_and_years() {
        assert_eq!(next_date("2026-09-28").as_deref(), Some("2026-09-29"));
        assert_eq!(next_date("2026-09-30").as_deref(), Some("2026-10-01"));
        assert_eq!(next_date("2026-12-31").as_deref(), Some("2027-01-01"));
        assert_eq!(next_date("2028-02-28").as_deref(), Some("2028-02-29"));
        assert_eq!(next_date("not-a-date"), None);
    }
}
