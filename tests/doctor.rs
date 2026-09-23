//! End-to-end tests for `mapbox doctor`.
//!
//! `--verify`'s connectivity check is the one part of this command that
//! makes a request, so it needs the same seam `tests/update_check.rs` uses
//! for the same reason: `MAPBOX_INTERNAL_DOCTOR_URL` points it at a loopback
//! server instead of the real `api.mapbox.com`, which is the only way to
//! test both the reachable and unreachable cases without depending on the
//! network being up (or down) when the suite runs.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("doctor-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config")).expect("create the scratch config dir");
    home
}

fn config_dir(home: &Path) -> PathBuf {
    home.join("config")
}

/// The real binary, isolated the same way the other command-level test
/// files isolate it — no token, no output mode, no config directory from
/// the developer's own environment.
fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("HTTPS_PROXY")
        .env_remove("HTTP_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("NO_PROXY")
        .env_remove("MAPBOX_CLI_NO_TELEMETRY")
        .env_remove("MAPBOX_NO_UPDATE_CHECK")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", config_dir(home));
    cmd
}

fn stdout(output: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("expected JSON, got {text:?} ({e})"))
}

/// A loopback stand-in for `api.mapbox.com`, answering every request with
/// the given status and closing.
fn server(status_line: &str) -> (std::thread::JoinHandle<()>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");
    let status_line = status_line.to_string();

    let handle = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        // Enough to know a request landed; the content doesn't matter here.
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let response = format!("{status_line}\r\nContent-Length: 0\r\n\r\n");
        let _ = stream.write_all(response.as_bytes());
    });

    (handle, format!("http://{addr}/"))
}

/// A port nothing is listening on, so a connection is refused rather than
/// merely slow — the unreachable case, answered quickly instead of by the
/// 5-second budget expiring.
fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");
    drop(listener);
    format!("http://{addr}/")
}

#[test]
fn with_no_token_and_nothing_configured() {
    let home = scratch("bare");

    let out = command(&home)
        .args(["-o", "json", "doctor"])
        .output()
        .expect("run mapbox doctor");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let value = stdout(&out);
    assert_eq!(value["token"]["available"], false);
    assert_eq!(value["proxy"]["active"], serde_json::json!([]));
    assert_eq!(value["switches"]["update_check_persisted"], true);
    assert_eq!(value["switches"]["update_check_env_opt_out"], false);
    assert_eq!(value["switches"]["telemetry_allowed"], true);
    assert!(
        value.get("connectivity").is_none(),
        "no --verify, no connectivity field"
    );
}

#[test]
fn a_token_in_the_environment_is_reported() {
    let home = scratch("env-token");

    let out = command(&home)
        .env("MAPBOX_ACCESS_TOKEN", "sk.eyJ1IjoiYWxpY2UifQ.sig")
        .args(["-o", "json", "doctor"])
        .output()
        .expect("run mapbox doctor");
    assert!(out.status.success());

    let value = stdout(&out);
    assert_eq!(value["token"]["available"], true);
    assert_eq!(value["token"]["source"], "environment");
    assert_eq!(value["token"]["account"], "alice");
    assert_eq!(value["token"]["usage"], "sk");
}

#[test]
fn the_persisted_and_environment_switches_are_both_reflected() {
    let home = scratch("switches");

    let set = command(&home)
        .args(["config", "set", "update-check", "off"])
        .output()
        .expect("run mapbox config set");
    assert!(set.status.success());

    let out = command(&home)
        .env("MAPBOX_NO_UPDATE_CHECK", "1")
        .env("MAPBOX_CLI_NO_TELEMETRY", "1")
        .args(["-o", "json", "doctor"])
        .output()
        .expect("run mapbox doctor");
    assert!(out.status.success());

    let value = stdout(&out);
    assert_eq!(value["switches"]["update_check_persisted"], false);
    assert_eq!(value["switches"]["update_check_env_opt_out"], true);
    assert_eq!(value["switches"]["telemetry_allowed"], false);
}

#[test]
fn a_proxy_variable_is_named() {
    let home = scratch("proxy");

    let out = command(&home)
        .env("HTTPS_PROXY", "http://localhost:9")
        .args(["-o", "json", "doctor"])
        .output()
        .expect("run mapbox doctor");
    assert!(out.status.success());

    let active = stdout(&out)["proxy"]["active"].clone();
    assert_eq!(active, serde_json::json!(["HTTPS_PROXY"]));
}

#[test]
fn without_verify_nothing_is_sent() {
    let home = scratch("no-verify");
    // A port nothing answers: if this were reached, the process would hang
    // for the budget's duration instead of returning immediately.
    let url = closed_port_url();

    let start = std::time::Instant::now();
    let out = command(&home)
        .env("MAPBOX_INTERNAL_DOCTOR_URL", &url)
        .args(["-o", "json", "doctor"])
        .output()
        .expect("run mapbox doctor");
    assert!(out.status.success());
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "doctor without --verify should not have touched the network at all"
    );
    assert!(stdout(&out).get("connectivity").is_none());
}

#[test]
fn verify_reports_a_reachable_host() {
    let home = scratch("verify-ok");
    let (thread, url) = server("HTTP/1.1 200 OK");

    let out = command(&home)
        .env("MAPBOX_INTERNAL_DOCTOR_URL", &url)
        .args(["-o", "json", "doctor", "--verify"])
        .output()
        .expect("run mapbox doctor --verify");
    assert!(out.status.success());

    let value = stdout(&out);
    assert_eq!(value["connectivity"]["reachable"], true);
    assert_eq!(value["connectivity"]["status"], 200);
    thread.join().expect("the loopback server thread");
}

#[test]
fn verify_reports_an_unreachable_host() {
    let home = scratch("verify-fail");
    let url = closed_port_url();

    let out = command(&home)
        .env("MAPBOX_INTERNAL_DOCTOR_URL", &url)
        .args(["-o", "json", "doctor", "--verify"])
        .output()
        .expect("run mapbox doctor --verify");
    assert!(
        out.status.success(),
        "a failed check is still a successful run"
    );

    let value = stdout(&out);
    assert_eq!(value["connectivity"]["reachable"], false);
}
