//! End-to-end tests for diagnostic logs and what `mapbox history show` says
//! about them.
//!
//! The unit tests in `src/run_log.rs` and `src/dated_jsonl.rs` cover the
//! pure parts — scrubbing a token out of a string, which lines a trim or a
//! shed keeps. What they cannot show is the contract across two stores: a
//! log only for a run history recorded, never with history off, dropped
//! without taking its history record along, and gone when its record is.
//!
//! Nothing here reaches the network. The one request a test makes goes to a
//! proxy on a loopback port nobody is listening on, so it fails at once and
//! is still a request `http::send` saw.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// A token-shaped fake. The signature is what must never reach the disk.
const TOKEN: &str = "pk.eyJ1IjoiZXhhbXBsZS11c2VyIiwiYSI6IngifQ.SIGNATURE-NOT-FOR-LOGS";

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("diagnostic-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the scratch home");
    home
}

fn config_dir(home: &Path) -> PathBuf {
    home.join(".mapbox")
}

fn log_dir(home: &Path) -> PathBuf {
    config_dir(home).join("logs")
}

fn history_dir(home: &Path) -> PathBuf {
    config_dir(home).join("history")
}

fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_HISTORY")
        .env_remove("MAPBOX_LOG")
        .env_remove("SUDO_USER")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .env("MAPBOX_NO_UPDATE_CHECK", "1")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", config_dir(home));
    cmd
}

fn run(home: &Path, args: &[&str]) -> Output {
    command(home).args(args).output().expect("run mapbox")
}

fn logged(home: &Path, args: &[&str]) -> Output {
    command(home)
        .env("MAPBOX_LOG", "1")
        .args(args)
        .output()
        .expect("run mapbox")
}

/// Every file's text under `dir`, concatenated in date order.
fn raw(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return String::new();
    };
    let mut files: Vec<PathBuf> = entries.map(|e| e.expect("an entry").path()).collect();
    files.sort();
    files
        .iter()
        .map(|f| std::fs::read_to_string(f).unwrap_or_default())
        .collect()
}

/// A request that fails before it leaves the machine: through a proxy on a
/// loopback port with nothing listening.
fn a_refused_request(home: &Path) -> Output {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        listener.local_addr().expect("the bound address").port()
    };
    command(home)
        .env("MAPBOX_LOG", "1")
        .env("HTTPS_PROXY", format!("http://127.0.0.1:{port}"))
        .args(["styles", "list", "--username", "example", "--token", TOKEN])
        .output()
        .expect("run mapbox")
}

fn show(home: &Path, id: Option<&str>) -> Value {
    let mut args = vec!["-o", "json", "history", "show"];
    args.extend(id);
    let out = run(home, &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("a JSON run")
}

/// Today's UTC date, as the files the binary just wrote are named.
fn today(home: &Path) -> String {
    std::fs::read_dir(history_dir(home))
        .expect("the history directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter_map(|name| name.strip_suffix(".jsonl").map(str::to_string))
        .max()
        .expect("today's history file")
}

/// The day before `date`, without a calendar crate.
fn day_before(date: &str) -> String {
    let (mut y, mut m, mut d): (u32, u32, u32) = (
        date[0..4].parse().unwrap(),
        date[5..7].parse().unwrap(),
        date[8..10].parse().unwrap(),
    );
    if d > 1 {
        d -= 1;
    } else {
        if m > 1 {
            m -= 1;
        } else {
            m = 12;
            y -= 1;
        }
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        d = match m {
            2 if leap => 29,
            2 => 28,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
    }
    format!("{y:04}-{m:02}-{d:02}")
}

#[test]
fn nothing_is_logged_unless_logging_was_turned_on() {
    let home = scratch("off");
    run(&home, &["styles", "lsit"]);
    assert!(!log_dir(&home).exists(), "logging off created logs/");
    assert_eq!(
        show(&home, None)["diagnostics"],
        serde_json::json!({ "status": "not_captured" })
    );
}

#[test]
fn a_logged_run_is_shown_with_its_record_and_keeps_no_token() {
    let home = scratch("captured");
    assert!(!a_refused_request(&home).status.success());

    let shown = show(&home, None);
    assert_eq!(shown["command"], serde_json::json!(["styles", "list"]));
    let diagnostics = &shown["diagnostics"];
    assert_eq!(diagnostics["status"], "captured", "{shown}");
    let log = &diagnostics["log"];
    assert_eq!(log["auth"]["source"], "flag");
    assert_eq!(log["auth"]["account"], "example-user");
    assert!(log["argv"].to_string().contains("<redacted>"), "{log}");
    let request = &log["requests"][0];
    assert!(request["status"].is_null(), "{request}");
    assert!(
        request["url"]
            .as_str()
            .unwrap()
            .contains("access_token=<redacted>"),
        "{request}"
    );

    let on_disk = raw(&log_dir(&home)) + &raw(&history_dir(&home));
    assert!(!on_disk.contains("SIGNATURE-NOT-FOR-LOGS"), "{on_disk}");
    assert!(!on_disk.contains(TOKEN), "{on_disk}");
}

#[test]
fn logging_needs_history() {
    let home = scratch("needs-history");
    let out = command(&home)
        .env("MAPBOX_HISTORY", "0")
        .env("MAPBOX_LOG", "1")
        .args(["styles", "lsit"])
        .output()
        .expect("run mapbox");
    assert!(!out.status.success());
    assert!(!config_dir(&home).exists(), "logging ran with history off");

    assert!(run(&home, &["config", "set", "history", "off"])
        .status
        .success());
    let refused = run(&home, &["-o", "json", "config", "set", "log", "on"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains(r#""code":"history_required""#),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let get = run(&home, &["-o", "text", "config", "get", "log"]);
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "off");
}

#[test]
fn a_log_that_is_gone_is_no_longer_available_and_its_record_stays() {
    let home = scratch("gone");
    logged(&home, &["styles", "lsit"]);
    let id = show(&home, None)["id"].as_str().unwrap().to_string();
    for entry in std::fs::read_dir(log_dir(&home)).expect("logs/") {
        std::fs::remove_file(entry.expect("an entry").path()).expect("remove a log file");
    }
    let shown = show(&home, Some(&id[..8]));
    assert_eq!(shown["id"], id.as_str(), "the record stays");
    assert_eq!(
        shown["diagnostics"],
        serde_json::json!({ "status": "unavailable" })
    );
}

#[test]
fn past_the_size_limit_the_oldest_logs_go_and_their_records_stay() {
    let home = scratch("limit");
    logged(&home, &["styles", "lsit"]);
    let yesterday = day_before(&today(&home));

    // A run from yesterday, with a log large enough to cross 100 MB alone.
    // Sparse: the size is in the metadata, which is all the limit reads.
    let old_id = "0ld00000-0000-4000-8000-000000000001";
    std::fs::write(
        history_dir(&home).join(format!("{yesterday}.jsonl")),
        format!(
            "{{\"id\":\"{old_id}\",\"time\":\"{yesterday}T12:00:00.000Z\",\"diagnosticsCaptured\":true}}\n"
        ),
    )
    .expect("yesterday's history");
    let big = std::fs::File::create(log_dir(&home).join(format!("{yesterday}.jsonl")))
        .expect("yesterday's log");
    big.set_len(101 * 1024 * 1024)
        .expect("a sparse 101 MB file");

    assert!(!logged(&home, &["styles", "lsit"]).status.success());
    let total: u64 = std::fs::read_dir(log_dir(&home))
        .expect("logs/")
        .map(|e| e.expect("an entry").metadata().expect("its size").len())
        .sum();
    assert!(total <= 100 * 1024 * 1024, "{total} bytes of logs");
    let old = show(&home, Some(old_id));
    assert_eq!(old["id"], old_id, "its history record stays");
    assert_eq!(old["diagnostics"]["status"], "unavailable");
    assert_eq!(
        show(&home, None)["diagnostics"]["status"],
        "captured",
        "the newest log stays"
    );
}

#[test]
fn a_log_goes_when_its_history_does() {
    let home = scratch("linked");
    logged(&home, &["styles", "lsit"]);
    let yesterday = day_before(&today(&home));

    // Detail for a day history no longer has, and a day past the window.
    let orphan = log_dir(&home).join(format!("{yesterday}.jsonl"));
    std::fs::write(&orphan, "{}\n").expect("an orphaned log");
    let expired_log = log_dir(&home).join("2000-01-01.jsonl");
    std::fs::write(history_dir(&home).join("2000-01-01.jsonl"), "{}\n").expect("expired history");
    std::fs::write(&expired_log, "{}\n").expect("an expired log");

    // With logging off, so the cleanup is not a side effect of writing.
    // History prunes itself only on the first run of a day, so the expired
    // history file may still be there; its log goes regardless.
    run(&home, &["styles", "lsit"]);
    assert!(!orphan.exists(), "a log outlived its history");
    assert!(!expired_log.exists(), "a log outlived 30 days");
    assert_eq!(
        show(&home, None)["diagnostics"]["status"],
        "not_captured",
        "the newest run, with logging off"
    );
}
