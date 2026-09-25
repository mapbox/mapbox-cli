//! End-to-end tests for `mapbox config get`/`set`.
//!
//! The unit tests in `src/config.rs` cover the pure decision — what an unset
//! key reads as, how the file round-trips. What they cannot show is that the
//! real binary's `get` sees what its own `set` wrote, through the config
//! directory rather than a value handed to a function in the same process.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("config-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config")).expect("create the scratch config dir");
    home
}

fn config_dir(home: &Path) -> PathBuf {
    home.join("config")
}

/// The real binary, isolated from the developer's own environment and
/// credential store the same way `tests/update_check.rs` isolates it.
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
fn an_unset_update_check_reads_on() {
    let home = scratch("get-default");

    // `-o text` explicitly: `auto`, the default, reads a piped stdout as a
    // request for JSON, which is exactly the other half of this test.
    let text = command(&home)
        .args(["-o", "text", "config", "get", "update-check"])
        .output()
        .expect("run mapbox config get");
    assert!(text.status.success());
    assert_eq!(stdout(&text), "on");

    let json = command(&home)
        .args(["-o", "json", "config", "get", "update-check"])
        .output()
        .expect("run mapbox config get -o json");
    assert!(json.status.success());
    assert_eq!(
        stdout(&json),
        r#"{"key":"update-check","value":true}"#,
        "no config.json exists yet, so this is the default reading itself back"
    );
    assert!(
        !config_dir(&home).join("config.json").exists(),
        "a bare `get` must not create the file a `set` would"
    );
}

#[test]
fn set_persists_across_separate_invocations() {
    let home = scratch("set-then-get");

    let off = command(&home)
        .args(["-o", "json", "config", "set", "update-check", "off"])
        .output()
        .expect("run mapbox config set");
    assert!(off.status.success());
    assert_eq!(stdout(&off), r#"{"key":"update-check","value":false}"#);

    // A fresh process, not the one that wrote it — the whole point being
    // tested is that the setting outlives a single invocation.
    let read_back = command(&home)
        .args(["-o", "text", "config", "get", "update-check"])
        .output()
        .expect("run mapbox config get");
    assert!(read_back.status.success());
    assert_eq!(stdout(&read_back), "off");

    let on = command(&home)
        .args(["-o", "text", "config", "set", "update-check", "on"])
        .output()
        .expect("run mapbox config set");
    assert!(on.status.success());
    assert_eq!(stdout(&on), "update-check set to on.");

    let read_back_again = command(&home)
        .args(["-o", "text", "config", "get", "update-check"])
        .output()
        .expect("run mapbox config get");
    assert_eq!(stdout(&read_back_again), "on");
}

#[test]
fn an_unknown_key_or_value_is_a_usage_error_not_a_panic() {
    let home = scratch("bad-input");

    let bad_key = command(&home)
        .args(["config", "get", "not-a-real-setting"])
        .output()
        .expect("run mapbox config get");
    assert!(!bad_key.status.success());

    let bad_value = command(&home)
        .args(["config", "set", "update-check", "sideways"])
        .output()
        .expect("run mapbox config set");
    assert!(!bad_value.status.success());
    assert!(
        !config_dir(&home).join("config.json").exists(),
        "a rejected value must not reach the file"
    );
}

#[test]
fn list_reports_every_setting_including_an_unset_one() {
    let home = scratch("list");

    // Nothing set yet: list still names every known key, at its default.
    let empty = command(&home)
        .args(["-o", "json", "config", "list"])
        .output()
        .expect("run mapbox config list");
    assert!(empty.status.success());
    assert_eq!(
        stdout(&empty),
        r#"[{"key":"update-check","value":true},{"key":"telemetry","value":true}]"#
    );

    let set = command(&home)
        .args(["config", "set", "update-check", "off"])
        .output()
        .expect("run mapbox config set");
    assert!(set.status.success());

    let after = command(&home)
        .args(["-o", "json", "config", "list"])
        .output()
        .expect("run mapbox config list");
    assert!(after.status.success());
    assert_eq!(
        stdout(&after),
        r#"[{"key":"update-check","value":false},{"key":"telemetry","value":true}]"#
    );

    let text = command(&home)
        .args(["-o", "text", "config", "list"])
        .output()
        .expect("run mapbox config list");
    assert!(text.status.success());
    assert_eq!(stdout(&text), "update-check\toff\ntelemetry\ton");
}

#[test]
fn unset_clears_the_key_rather_than_writing_the_default() {
    let home = scratch("unset");
    let path = config_dir(&home).join("config.json");

    // Set to `on` explicitly — the default value, but written, not absent.
    let set = command(&home)
        .args(["config", "set", "update-check", "on"])
        .output()
        .expect("run mapbox config set");
    assert!(set.status.success());
    let written = std::fs::read_to_string(&path).expect("config.json exists");
    assert!(
        written.contains("update_check"),
        "an explicit `on` should still be written: {written:?}"
    );

    let unset = command(&home)
        .args(["-o", "json", "config", "unset", "update-check"])
        .output()
        .expect("run mapbox config unset");
    assert!(unset.status.success());
    assert_eq!(stdout(&unset), r#"{"key":"update-check","value":true}"#);

    let cleared = std::fs::read_to_string(&path).expect("config.json still exists");
    assert_eq!(
        cleared, "{}",
        "unset should remove the key, not merely write back its default"
    );
}

#[cfg(unix)]
#[test]
fn the_config_file_is_written_private() {
    use std::os::unix::fs::PermissionsExt;

    let home = scratch("perms");
    let out = command(&home)
        .args(["config", "set", "update-check", "off"])
        .output()
        .expect("run mapbox config set");
    assert!(out.status.success());

    let mode = std::fs::metadata(config_dir(&home).join("config.json"))
        .expect("config.json exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o600,
        "config.json should be as private as credentials.json"
    );
}
