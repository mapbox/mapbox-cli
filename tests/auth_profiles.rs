//! End-to-end tests for `mapbox auth profiles`.
//!
//! The reverse-filename-parsing logic is unit-tested in `src/auth.rs`. What
//! that cannot show is the real binary reading a real config directory: the
//! default profile alongside named ones, a profile with no `exp` claim next
//! to one that has one, and files that must not be mistaken for a profile at
//! all — `config.json`, a lock file, `update-check.json`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("auth-profiles-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".mapbox")).expect("create the scratch config dir");
    home
}

fn config_dir(home: &Path) -> PathBuf {
    home.join(".mapbox")
}

/// A token shaped the way this crate's own decoders expect:
/// `<usage>.<base64url payload>.<signature>` — see `token_account` and
/// `token_expires_at` in `src/auth.rs`, which read position 1 as the
/// payload. `exp` is omitted entirely when `None`, the same as a `pk`/`sk`
/// token that carries no expiry.
fn token(account: &str, exp: Option<u64>) -> String {
    let payload = match exp {
        Some(exp) => format!(r#"{{"u":"{account}","exp":{exp}}}"#),
        None => format!(r#"{{"u":"{account}"}}"#),
    };
    format!("sk.{}.sig", URL_SAFE_NO_PAD.encode(payload))
}

fn write_credentials(dir: &Path, filename: &str, account: &str, exp: Option<u64>) {
    let json = format!(
        r#"{{"access_token":"{}","username":"{account}"}}"#,
        token(account, exp)
    );
    std::fs::write(dir.join(filename), json).expect("write a fake credentials file");
}

fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", config_dir(home));
    cmd
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

#[test]
fn no_stored_profiles_says_so_rather_than_an_empty_table() {
    let home = scratch("none");

    let text = command(&home)
        .args(["-o", "text", "auth", "profiles"])
        .output()
        .expect("run mapbox auth profiles");
    assert!(text.status.success());
    assert!(
        stdout(&text).contains("No stored profiles"),
        "{}",
        stdout(&text)
    );

    let json = command(&home)
        .args(["-o", "json", "auth", "profiles"])
        .output()
        .expect("run mapbox auth profiles -o json");
    assert!(json.status.success());
    assert_eq!(stdout(&json), "[]");
}

#[test]
fn every_stored_profile_is_listed_default_first() {
    let home = scratch("populated");
    let dir = config_dir(&home);
    write_credentials(&dir, "credentials.json", "alice", Some(4_102_444_800));
    write_credentials(&dir, "credentials-work.json", "bob-work", None);
    // A profile named so it sorts before "default" alphabetically, to prove
    // the ordering is deliberate rather than incidentally alphabetical.
    write_credentials(&dir, "credentials-acme.json", "acme-bot", None);

    let json = command(&home)
        .args(["-o", "json", "auth", "profiles"])
        .output()
        .expect("run mapbox auth profiles -o json");
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );

    let parsed: serde_json::Value = serde_json::from_str(&stdout(&json)).expect("valid JSON");
    let entries = parsed.as_array().expect("a JSON array");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["profile"], "default");
    assert_eq!(entries[0]["account"], "alice");
    assert!(entries[0]["expires_at"].as_u64().is_some());
    assert_eq!(entries[1]["profile"], "acme");
    assert_eq!(entries[2]["profile"], "work");
    assert_eq!(entries[2]["account"], "bob-work");
    assert!(entries[2]["expires_at"].is_null());
}

#[test]
fn only_credentials_files_count_as_a_profile() {
    let home = scratch("noise");
    let dir = config_dir(&home);
    write_credentials(&dir, "credentials.json", "alice", None);
    // Everything a real config directory can hold that is not a profile.
    std::fs::write(dir.join("credentials.json.lock"), "").unwrap();
    std::fs::write(dir.join("credentials-work.json.lock"), "").unwrap();
    std::fs::write(dir.join("config.json"), r#"{"update_check":false}"#).unwrap();
    std::fs::write(dir.join("update-check.json"), "{}").unwrap();

    let json = command(&home)
        .args(["-o", "json", "auth", "profiles"])
        .output()
        .expect("run mapbox auth profiles -o json");
    assert!(json.status.success());
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&json)).expect("valid JSON");
    let entries = parsed.as_array().expect("a JSON array");
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["profile"], "default");
}

#[test]
fn an_absent_config_directory_lists_nothing_and_creates_none() {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("auth-profiles-absent");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the scratch home, with no .mapbox inside it");

    let json = command(&home)
        .args(["-o", "json", "auth", "profiles"])
        .output()
        .expect("run mapbox auth profiles -o json");
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    assert_eq!(stdout(&json), "[]");
    assert!(
        !config_dir(&home).exists(),
        "listing profiles must not create the config directory"
    );
}
