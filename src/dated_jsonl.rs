//! Private, append-only, one-file-per-UTC-day JSONL directories under the
//! config directory, pruned to a fixed number of days, for the consumers of
//! [`crate::run_record`] that keep records on disk. The only files this
//! deletes are ones named exactly `YYYY-MM-DD.jsonl` inside the directory
//! it was handed.

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

/// Every line in `dir`'s dated files, oldest first. A missing directory is
/// no lines.
pub(crate) fn read_all(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| dated_file(name).is_some())
        .collect();
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
}
