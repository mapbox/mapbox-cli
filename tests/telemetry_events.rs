//! End-to-end tests for the run's `cli.command` event.
//!
//! The unit tests in `src/telemetry_event.rs` cover the pure parts — how an
//! argument is classified, what a timestamp looks like. What they cannot show is what a real run leaves behind: that the
//! event lands where it should, carries what the command did and nothing the
//! user typed, disappears when telemetry is off, and never changes stdout.
//!
//! `MAPBOX_INTERNAL_TELEMETRY_URL` points the binary at a loopback server,
//! which is where these tests read the event. The CLI's token endpoint is
//! pointed at a dead port, and a run with no token drops its event, so most
//! runs here are given `MAPBOX_CLI_TOKEN` to send with. The tests of the
//! fetched token point `MAPBOX_INTERNAL_CLI_TOKEN_URL`, and the Tokens API
//! check behind `MAPBOX_INTERNAL_TOKENS_URL`, at loopback servers.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

/// A token whose payload claims the account `example-user`. The signature
/// is the part that must never appear in an event.
const TOKEN: &str = "pk.eyJ1IjoiZXhhbXBsZS11c2VyIiwiYSI6IngifQ.SIGNATURE-NOT-FOR-EVENTS";
const ADDRESS: &str = "1600 Pennsylvania Ave";

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("events-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    // With `.mapbox` already there, as on any machine that has logged in:
    // recording never creates it.
    std::fs::create_dir_all(config_dir(&home)).expect("create the scratch config dir");
    home
}

fn config_dir(home: &Path) -> PathBuf {
    home.join(".mapbox")
}

fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_CLI_NO_TELEMETRY")
        .env_remove("MAPBOX_INTERNAL_TELEMETRY_URL")
        .env_remove("MAPBOX_CLI_TOKEN")
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", "http://127.0.0.1:9/")
        .env("MAPBOX_INTERNAL_TOKENS_URL", "http://127.0.0.1:9/")
        .env_remove("MAPBOX_INTERNAL_TELEMETRY_SEND")
        .env_remove("MAPBOX_INTERNAL_TELEMETRY_LOG")
        .env_remove("SUDO_USER")
        .env("MAPBOX_NO_UPDATE_CHECK", "1")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", config_dir(home));
    cmd
}

fn run(home: &Path, args: &[&str]) -> Output {
    command(home).args(args).output().expect("run mapbox")
}

/// Runs `cmd` with its event sent to a loopback server, and returns the run
/// and the event the server received. `MAPBOX_CLI_TOKEN` gives the run a
/// token to send with whether or not it has one of its own.
fn sent(cmd: &mut Command) -> (Output, Value) {
    let (received, url) = events_server(true);
    let out = cmd
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_CLI_TOKEN", "pk.cli")
        .output()
        .expect("run mapbox");
    let request = received
        .recv_timeout(Duration::from_secs(10))
        .expect("the event was posted");
    let batch: Value = serde_json::from_str(&request.body).expect("the body is JSON");
    (out, batch[0].clone())
}

fn run_sent(home: &Path, args: &[&str]) -> (Output, Value) {
    sent(command(home).args(args))
}

#[test]
fn a_run_writes_one_event_with_what_it_did() {
    let home = scratch("one");
    let data = r#"{"name":"secret-style-name","layers":[]}"#;
    let (out, event) = run_sent(
        &home,
        &[
            "styles",
            "create",
            "--username",
            "someone",
            "--data",
            data,
            "--dry-run",
            "-t",
            TOKEN,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(event["event"], "cli.command");
    assert_eq!(event["sdkIdentifier"], "mapbox-cli");
    assert_eq!(event["command"], serde_json::json!(["styles", "create"]));
    assert_eq!(event["invocation"], "execute");
    assert_eq!(event["exitCode"], 0);
    assert_eq!(event["dryRun"], true);
    assert_eq!(event["auth"]["source"], "flag");
    assert_eq!(event["auth"]["type"], "pk");
    assert_eq!(event["auth"]["account"], "example-user");
    assert_eq!(
        event["stdoutBytes"].as_u64(),
        Some(out.stdout.len() as u64),
        "stdoutBytes should be what was written"
    );
    let params = event["params"].as_array().expect("params");
    assert!(params.contains(&serde_json::json!({
        "name": "data", "bytes": data.len(), "keys": ["layers", "name"]
    })));
    assert!(params.contains(&serde_json::json!({ "name": "username", "length": 7 })));
}

#[test]
fn nothing_the_user_typed_reaches_the_event() {
    let home = scratch("private");
    // No network needed: a usage error still records, and `--dry-run` sends
    // nothing.
    let (_, first) = run_sent(
        &home,
        &[
            "styles",
            "create",
            "--username",
            "someone",
            "--data",
            "{\"x\":1}",
            "--dry-run",
            "-t",
            TOKEN,
        ],
    );
    // A request that parses and then fails offline, through a proxy that
    // isn't there, so `--q` is classified rather than lost to a usage error.
    let (_, second) = sent(
        command(&home)
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env_remove("ALL_PROXY")
            .env_remove("NO_PROXY")
            .args(["geocoder", "forward", "--q", ADDRESS, "-t", TOKEN]),
    );
    assert!(
        second["params"]
            .as_array()
            .expect("params")
            .contains(&serde_json::json!({ "name": "q", "length": ADDRESS.chars().count() })),
        "{second}"
    );
    let (_, third) = run_sent(&home, &["/Users/someone/secret/path"]);

    let written = [first, second, third].map(|e| e.to_string()).concat();
    for secret in [
        "SIGNATURE-NOT-FOR-EVENTS",
        ADDRESS,
        "someone",
        "secret/path",
    ] {
        assert!(
            !written.contains(secret),
            "`{secret}` reached an event:\n{written}"
        );
    }
}

#[test]
fn help_version_and_usage_errors_record_their_invocation() {
    let home = scratch("invocation");
    let events = [
        &["--version"][..],
        &["styles", "--help"],
        &["styles", "list", "--schema"],
        &["nosuchcommand"],
    ]
    .map(|args| run_sent(&home, args).1);
    let seen: Vec<(&str, &Value)> = events
        .iter()
        .map(|e| (e["invocation"].as_str().unwrap_or(""), &e["command"]))
        .collect();
    assert_eq!(
        seen,
        [
            ("version", &Value::Null),
            ("help", &serde_json::json!(["styles"])),
            ("schema", &serde_json::json!(["styles", "list"])),
            ("execute", &Value::Null),
        ]
    );
    assert_eq!(events[3]["usageError"], "InvalidSubcommand");
    assert_eq!(events[3]["errorCode"], "usage");
    assert_eq!(events[3]["exitCode"], 2);
}

#[test]
fn either_opt_out_records_nothing() {
    let home = scratch("opt-out-env");
    let out = command(&home)
        .env("MAPBOX_CLI_NO_TELEMETRY", "1")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert!(
        !config_dir(&home).join(".telemetry").exists(),
        "MAPBOX_CLI_NO_TELEMETRY=1 still wrote telemetry"
    );

    // With a token and somewhere to send, so that nothing arriving is the
    // setting's doing.
    let home = scratch("opt-out-config");
    let (received, url) = events_server(true);
    let with_a_token = |args: &[&str]| {
        command(&home)
            .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
            .env("MAPBOX_CLI_TOKEN", "pk.cli")
            .args(args)
            .output()
            .expect("run mapbox")
    };
    assert!(with_a_token(&["config", "set", "telemetry", "off"])
        .status
        .success());
    // That run sends nothing either: the setting it wrote is read at exit.
    let _ = with_a_token(&["config", "list"]);
    assert!(
        received.recv_timeout(Duration::from_secs(3)).is_err(),
        "`telemetry off` still sent an event"
    );
    // Not left to the send child's own check: the run itself records nothing.
    assert!(
        !config_dir(&home).join(".telemetry").exists(),
        "`telemetry off` still wrote telemetry"
    );
}

#[test]
fn stdout_is_identical_with_telemetry_on_and_off() {
    // On with a token and somewhere to send, so the child really runs.
    let (on, _) = run_sent(&scratch("stdout-on"), &["-o", "json", "config", "list"]);
    let off = command(&scratch("stdout-off"))
        .env("MAPBOX_CLI_NO_TELEMETRY", "1")
        .args(["-o", "json", "config", "list"])
        .output()
        .expect("run mapbox");
    assert_eq!(on.stdout, off.stdout);
    assert_eq!(on.stderr, off.stderr);
}

#[test]
fn completion_records_nothing() {
    let home = scratch("completion");
    assert!(run(&home, &["completion", "zsh"]).status.success());
    assert!(
        !config_dir(&home).join(".telemetry").exists(),
        "`completion` wrote telemetry"
    );
}

/// Recording never creates the config directory: a machine that has never
/// logged in or set a config keeps no `~/.mapbox` at all.
#[test]
fn without_a_config_directory_nothing_is_created() {
    let home = scratch("no-config-dir");
    std::fs::remove_dir(config_dir(&home)).expect("remove the scratch config dir");
    assert!(run(&home, &["styles", "--help"]).status.success());
    assert!(
        !config_dir(&home).exists(),
        "recording created {}",
        config_dir(&home).display()
    );
}

#[test]
fn a_run_started_by_a_workflow_step_records_its_parent() {
    let parent = "5f0c1e9a-7b2d-4c1e-9f3a-2d8e6b1a0c47";
    let home = scratch("parent");
    let (out, first) = sent(
        command(&home)
            .env("MAPBOX_CLI_PARENT_EVENT", parent)
            .args(["config", "list"]),
    );
    assert!(out.status.success());
    // Anything that isn't an event id is ignored rather than recorded.
    let (_, second) = sent(
        command(&home)
            .env("MAPBOX_CLI_PARENT_EVENT", "/Users/someone/secret")
            .args(["config", "list"]),
    );

    let events = [first, second];
    assert_eq!(events[0]["parentEventId"], parent);
    assert_ne!(events[0]["eventId"], parent);
    assert!(events[1].get("parentEventId").is_none(), "{:?}", events[1]);
}

/// One request as the loopback server saw it.
struct Received {
    head: String,
    body: String,
}

/// A loopback stand-in for Mapbox Events. Reports the first request it gets,
/// and answers it only when `respond` is set — otherwise it holds the
/// connection open, as an unreachable service would.
fn events_server(respond: bool) -> (mpsc::Receiver<Received>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let length = head
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        let _ = stream.read_exact(&mut body);
        let body = String::from_utf8_lossy(&body).into_owned();
        let _ = sender.send(Received { head, body });
        if respond {
            let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        } else {
            std::thread::sleep(Duration::from_secs(30));
        }
    });
    (receiver, format!("http://{addr}/events/v2"))
}

#[test]
fn with_a_send_url_the_event_is_posted() {
    let home = scratch("send");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_ACCESS_TOKEN", "pk.test")
        // A marker `http::client` would add, which must not reach Mapbox Events.
        .env("CLAUDECODE", "1")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());

    let request = received
        .recv_timeout(Duration::from_secs(10))
        .expect("the event was posted");
    let request_line = request.head.lines().next().unwrap_or_default();
    assert_eq!(
        request_line,
        "POST /events/v2?access_token=pk.test HTTP/1.1"
    );
    let user_agent = request
        .head
        .lines()
        .find_map(|l| l.strip_prefix("user-agent: "))
        .expect("a user agent");
    assert_eq!(
        user_agent,
        concat!("mapbox-cli/", env!("CARGO_PKG_VERSION")),
        "the user agent should be the product token alone"
    );

    let batch: Value = serde_json::from_str(&request.body).expect("the body is JSON");
    let batch = batch.as_array().expect("the body is a batch");
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0]["event"], "cli.command");
    assert_eq!(batch[0]["command"], serde_json::json!(["config", "list"]));
}

#[test]
fn the_command_does_not_wait_for_the_send() {
    let home = scratch("send-hangs");
    let (received, url) = events_server(false);
    let started = Instant::now();
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_ACCESS_TOKEN", "pk.test")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    let elapsed = started.elapsed();
    assert!(out.status.success());
    // The server proves the send was under way while the command returned.
    assert!(received.recv_timeout(Duration::from_secs(10)).is_ok());
    assert!(
        elapsed < Duration::from_secs(3),
        "the command waited {elapsed:?} on a send that never answers"
    );
}

#[test]
fn the_opt_out_sends_nothing() {
    let home = scratch("send-opt-out");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_ACCESS_TOKEN", "pk.test")
        .env("MAPBOX_CLI_NO_TELEMETRY", "1")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert!(
        received.recv_timeout(Duration::from_secs(3)).is_err(),
        "MAPBOX_CLI_NO_TELEMETRY=1 still sent an event"
    );
}

/// `sudo` keeps `HOME`, so a root run would leave root-owned files in the
/// user's `~/.mapbox`.
#[test]
fn a_run_under_sudo_sends_nothing() {
    let home = scratch("send-sudo");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_ACCESS_TOKEN", "pk.test")
        .env("SUDO_USER", "someone")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert!(
        received.recv_timeout(Duration::from_secs(3)).is_err(),
        "a run under sudo still sent an event"
    );
    assert!(!config_dir(&home).join(".telemetry").exists());
}

/// Every line of the delivery log under `home`, waiting up to ten seconds
/// for `count` of them: the outcome is written by a child the command did
/// not wait for.
fn deliveries(home: &Path, count: usize) -> Vec<Value> {
    let dir = config_dir(home).join(".telemetry").join("deliveries");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let lines: Vec<Value> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flat_map(|entry| {
                let text =
                    std::fs::read_to_string(entry.expect("an entry").path()).unwrap_or_default();
                text.lines()
                    .map(|l| serde_json::from_str(l).expect("a delivery line is JSON"))
                    .collect::<Vec<Value>>()
            })
            .collect();
        if lines.len() >= count || Instant::now() > deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_delivery_log_pairs_each_hand_off_with_its_response() {
    let home = scratch("delivery-log");
    let (_received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_ACCESS_TOKEN", "pk.test")
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());

    let lines = deliveries(&home, 2);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["stage"], "queued");
    assert_eq!(lines[1]["stage"], "responded");
    assert_eq!(lines[1]["status"], 204);
    assert!(lines[0]["eventId"].is_string());
    assert_eq!(lines[0]["eventId"], lines[1]["eventId"]);
    let logged = lines.iter().map(Value::to_string).collect::<String>();
    assert!(
        !logged.contains("pk.test"),
        "the delivery log holds the token: {logged}"
    );
}

#[test]
fn the_delivery_log_says_why_a_send_failed() {
    let home = scratch("delivery-log-failed");
    // A port that was bound and released: nothing is listening.
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a loopback port")
        .port();
    let out = command(&home)
        .env(
            "MAPBOX_INTERNAL_TELEMETRY_URL",
            format!("http://127.0.0.1:{port}/events/v2"),
        )
        .env("MAPBOX_ACCESS_TOKEN", "pk.test")
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());

    let lines = deliveries(&home, 2);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["stage"], "failed");
    assert!(
        lines[1]["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("could not connect")),
        "{lines:?}"
    );
}

#[test]
fn without_the_switch_there_is_no_delivery_log() {
    let home = scratch("no-delivery-log");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_ACCESS_TOKEN", "pk.test")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert!(received.recv_timeout(Duration::from_secs(10)).is_ok());
    // The child logs after the response; give it time to have done so.
    std::thread::sleep(Duration::from_millis(500));
    assert!(!config_dir(&home)
        .join(".telemetry")
        .join("deliveries")
        .exists());
}

#[test]
fn a_typed_token_reaches_the_send() {
    let home = scratch("send-typed-token");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .args(["--token", "pk.typed", "config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    let request = received
        .recv_timeout(Duration::from_secs(10))
        .expect("the event was posted");
    assert_eq!(
        request.head.lines().next().unwrap_or_default(),
        "POST /events/v2?access_token=pk.typed HTTP/1.1"
    );
}

/// Telemetry may fall back to the CLI's token whatever URL it is sent to, so
/// an overridden one keeps it.
#[test]
fn the_clis_token_sends_the_event_for_someone_with_none() {
    let home = scratch("send-cli-token-only");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_CLI_TOKEN", "pk.cli")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    let request = received
        .recv_timeout(Duration::from_secs(10))
        .expect("the event was posted");
    assert_eq!(
        request.head.lines().next().unwrap_or_default(),
        "POST /events/v2?access_token=pk.cli HTTP/1.1"
    );
}

#[test]
fn use_login_keeps_the_environment_token_out_of_the_send() {
    let home = scratch("send-use-login");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_ACCESS_TOKEN", "pk.env")
        .env("MAPBOX_CLI_TOKEN", "pk.cli")
        .args(["--use-login", "config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(
        sent_with(&received),
        "pk.cli",
        "--use-login still sent MAPBOX_ACCESS_TOKEN"
    );
}

/// The `access_token` the loopback server's one request carried.
fn sent_with(received: &mpsc::Receiver<Received>) -> String {
    let request = received
        .recv_timeout(Duration::from_secs(10))
        .expect("the event was posted");
    let line = request.head.lines().next().unwrap_or_default().to_string();
    line.split("access_token=")
        .nth(1)
        .and_then(|rest| rest.split([' ', '&']).next())
        .unwrap_or_default()
        .to_string()
}

#[test]
fn the_users_token_comes_before_the_clis() {
    let home = scratch("send-user-first");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_CLI_TOKEN", "pk.cli")
        .args(["--token", "pk.typed", "config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.typed");
}

/// `--version`, help and usage errors return before the arguments are parsed,
/// and still send with the environment's token rather than the CLI's.
#[test]
fn a_run_that_never_parses_still_sends_with_the_environment_token() {
    for args in [
        &["--version"][..],
        &["styles", "--help"],
        &["nosuchcommand"],
    ] {
        let home = scratch("send-unparsed");
        let (received, url) = events_server(true);
        let _ = command(&home)
            .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
            .env("MAPBOX_ACCESS_TOKEN", "pk.env")
            .env("MAPBOX_CLI_TOKEN", "pk.cli")
            .args(args)
            .output()
            .expect("run mapbox");
        assert_eq!(sent_with(&received), "pk.env", "{args:?}");
    }
}

#[test]
fn a_run_that_never_parses_still_honors_use_login() {
    for args in [
        &["--use-login", "--version"][..],
        &["--use-login", "styles", "--help"],
        &["--use-login", "nosuchcommand"],
    ] {
        let home = scratch("send-unparsed-use-login");
        let (received, url) = events_server(true);
        let _ = command(&home)
            .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
            .env("MAPBOX_ACCESS_TOKEN", "pk.env")
            .env("MAPBOX_CLI_TOKEN", "pk.cli")
            .args(args)
            .output()
            .expect("run mapbox");
        assert_eq!(sent_with(&received), "pk.cli", "{args:?}");
    }
}

/// `MapboxAccessToken` is the user's token for `tilesets-cli`, so it comes
/// before the CLI's even though clap doesn't read it.
#[test]
fn the_legacy_token_variable_is_the_users_too() {
    for args in [&["config", "list"][..], &["--version"]] {
        let home = scratch("send-legacy-env");
        let (received, url) = events_server(true);
        let _ = command(&home)
            .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
            .env("MapboxAccessToken", "pk.legacy")
            .env("MAPBOX_CLI_TOKEN", "pk.cli")
            .args(args)
            .output()
            .expect("run mapbox");
        assert_eq!(sent_with(&received), "pk.legacy", "{args:?}");
    }
}

/// The child's login step reads without the lock while the login is good, so
/// a read-only command's telemetry neither creates a lock file nor tightens
/// the directory's mode.
#[cfg(unix)]
#[test]
fn sending_with_a_login_leaves_the_config_directory_alone() {
    use std::os::unix::fs::PermissionsExt;

    let home = scratch("send-login-readonly");
    let dir = config_dir(&home);
    // No `exp`, so it never needs a refresh.
    std::fs::write(
        dir.join("credentials.json"),
        r#"{"access_token":"pk.login"}"#,
    )
    .expect("write a login");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.login");

    let mode = std::fs::metadata(&dir)
        .expect("the config dir")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755, "the send changed the directory's mode");
    assert!(
        !dir.join("credentials.json.lock").exists(),
        "the send took the credentials lock"
    );
}

/// With a token to send with, the child runs; it still must not be what
/// creates `~/.mapbox` on a machine that has none.
#[test]
fn sending_never_creates_the_config_directory() {
    let home = scratch("send-no-config-dir");
    std::fs::remove_dir(config_dir(&home)).expect("remove the scratch config dir");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_CLI_TOKEN", "pk.cli")
        .args(["styles", "--help"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.cli");
    assert!(!config_dir(&home).exists(), "the send created ~/.mapbox");
}

#[test]
fn without_a_token_the_event_is_dropped() {
    let home = scratch("no-token");
    // The token endpoint is unreachable, which only the child finds out.
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    let lines = deliveries(&home, 2);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["stage"], "queued");
    assert_eq!(lines[1]["error"], "no token to send with");
    let kept: Vec<PathBuf> = std::fs::read_dir(config_dir(&home).join(".telemetry"))
        .expect("the telemetry directory")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert!(kept.is_empty(), "the event was kept on disk: {kept:?}");
}

/// A server that answers one request with `status` and `body`, and reports
/// that it was asked.
fn answer_once(status: &'static str, body: &'static str) -> (mpsc::Receiver<()>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => break,
            }
        }
        // Read the body too: closing with it unread resets the connection
        // before the client sees the answer.
        let head = String::from_utf8_lossy(&head).into_owned();
        let length = head
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        let _ = stream.read_exact(&mut vec![0u8; length]);
        let _ = sender.send(());
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    (receiver, format!("http://{addr}/config/token.json"))
}

/// A server that answers one request after another with `answers`, in
/// order, and passes on each request's head.
fn answer_each(
    answers: &'static [(&'static str, &'static str)],
) -> (mpsc::Receiver<String>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in answers {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let length = head
                .lines()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            let _ = stream.read_exact(&mut vec![0u8; length]);
            let _ = sender.send(head);
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (receiver, format!("http://{addr}/events/v2"))
}

/// Waits for the child to have written the CLI token's state, which happens
/// after the run itself has exited.
fn cli_token_state(home: &Path, contains: &str) -> Value {
    let path = config_dir(home).join("cli-token.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if text.contains(contains) {
                return serde_json::from_str(&text).expect("the state is JSON");
            }
        }
        assert!(Instant::now() < deadline, "no state holding {contains}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// With no token of the user's and no `MAPBOX_CLI_TOKEN`, the event goes
/// with the token fetched from the endpoint, which is kept for the next run.
#[test]
fn the_fetched_token_sends_the_event_and_is_kept() {
    let home = scratch("send-fetched");
    let (asked, token_url) = answer_once("200 OK", r#"{"token":"pk.fetched"}"#);
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.fetched");
    asked
        .recv_timeout(Duration::from_secs(10))
        .expect("the endpoint was asked");
    assert_eq!(cli_token_state(&home, "pk.fetched")["token"], "pk.fetched");

    // The endpoint is gone now; the kept token is enough.
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.fetched");
}

/// A `401` to the kept token, which the Tokens API says is revoked, marks
/// it; a new one is fetched at once and the event sent again with it.
#[test]
fn a_revoked_token_is_replaced_and_the_event_resent() {
    let home = scratch("send-rejected");
    std::fs::write(
        config_dir(&home).join("cli-token.json"),
        r#"{"token":"pk.old","overridden":true}"#,
    )
    .expect("write the state");
    let (heads, events_url) = answer_each(&[
        ("401 Unauthorized", r#"{"message":"Not Authorized"}"#),
        ("204 No Content", ""),
    ]);
    // A `401` around the verdict, which the real endpoint has not been seen
    // to send: the `code` still decides.
    let (_, tokens_url) = answer_once("401 Unauthorized", r#"{"code":"TokenRevoked"}"#);
    let (_, token_url) = answer_once("200 OK", r#"{"token":"pk.new"}"#);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &events_url)
        .env("MAPBOX_INTERNAL_TOKENS_URL", &tokens_url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    let tokens: Vec<String> = (0..2)
        .map(|_| {
            let head = heads
                .recv_timeout(Duration::from_secs(10))
                .expect("an event was posted");
            head.split("access_token=")
                .nth(1)
                .and_then(|rest| rest.split([' ', '&']).next())
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(tokens, ["pk.old", "pk.new"]);
    assert_eq!(cli_token_state(&home, "pk.new")["rejected"], false);
}

/// A `401` to a token the Tokens API calls valid is this API refusing it,
/// not the token being dead, so it is kept.
#[test]
fn a_token_an_api_refuses_is_kept() {
    let home = scratch("send-forbidden");
    std::fs::write(
        config_dir(&home).join("cli-token.json"),
        r#"{"token":"pk.kept","overridden":true}"#,
    )
    .expect("write the state");
    let (_, events_url) = answer_once("401 Unauthorized", r#"{"message":"Not Authorized"}"#);
    let (checked, tokens_url) = answer_once("200 OK", r#"{"code":"TokenValid"}"#);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .env(
            "MAPBOX_INTERNAL_TELEMETRY_URL",
            events_url.replace("/config/token.json", "/events/v2"),
        )
        .env("MAPBOX_INTERNAL_TOKENS_URL", &tokens_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    checked
        .recv_timeout(Duration::from_secs(10))
        .expect("the token was checked");
    let lines = deliveries(&home, 2);
    assert_eq!(lines[1]["status"], 401, "{lines:?}");
    let state = cli_token_state(&home, "cooldowns");
    assert_eq!(state["token"], "pk.kept");
    assert_eq!(state["rejected"], false);
    assert_eq!(state["cooldowns"]["telemetry"]["failures"], 1);

    // Resting: the next run sends nothing with it, and checks nothing.
    let (sent, events_url) = answer_once("401 Unauthorized", r#"{"message":"Not Authorized"}"#);
    let (checked, tokens_url) = answer_once("200 OK", r#"{"code":"TokenValid"}"#);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .env(
            "MAPBOX_INTERNAL_TELEMETRY_URL",
            events_url.replace("/config/token.json", "/events/v2"),
        )
        .env("MAPBOX_INTERNAL_TOKENS_URL", &tokens_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    let lines = deliveries(&home, 3);
    assert_eq!(lines[2]["stage"], "no_token", "{lines:?}");
    assert!(
        sent.recv_timeout(Duration::from_millis(500)).is_err(),
        "sent while resting"
    );
    assert!(
        checked.recv_timeout(Duration::from_millis(100)).is_err(),
        "checked while resting"
    );
}

/// The endpoint still serving the rejected token is not asked again for an
/// hour, and the event is dropped meanwhile.
#[test]
fn the_rejected_token_served_again_waits_before_asking() {
    let home = scratch("send-still-rejected");
    std::fs::write(
        config_dir(&home).join("cli-token.json"),
        r#"{"token":"pk.old","rejected":true,"overridden":true}"#,
    )
    .expect("write the state");
    let (_, token_url) = answer_once("200 OK", r#"{"token":"pk.old"}"#);
    let out = command(&home)
        .env(
            "MAPBOX_INTERNAL_TELEMETRY_URL",
            "http://127.0.0.1:1/events/v2",
        )
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    let state = cli_token_state(&home, "retry_at");
    assert_eq!(state["failures"], 1);

    // Nothing listens at the endpoint now, and nothing should try it.
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .env(
            "MAPBOX_INTERNAL_TELEMETRY_URL",
            "http://127.0.0.1:1/events/v2",
        )
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    let lines = deliveries(&home, 1);
    assert_eq!(lines.last().expect("a delivery line")["stage"], "no_token");
}

/// With no config directory, a fetched token still has to be kept, or a dead
/// one is never marked and nothing backs off; so the directory is created.
#[test]
fn a_fetched_token_is_kept_even_without_a_config_directory() {
    let home = scratch("send-fetched-no-dir");
    std::fs::remove_dir(config_dir(&home)).expect("remove the scratch config dir");
    let (_, token_url) = answer_once("200 OK", r#"{"token":"pk.fetched"}"#);
    let (received, url) = events_server(true);
    // History would create the directory first, and hide whether keeping
    // the token does.
    let out = command(&home)
        .env("MAPBOX_HISTORY", "0")
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.fetched");
    assert_eq!(cli_token_state(&home, "pk.fetched")["token"], "pk.fetched");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(config_dir(&home))
            .expect("the config dir")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}

/// An existing config directory is written into as it is.
#[cfg(unix)]
#[test]
fn keeping_a_fetched_token_leaves_the_directory_mode_alone() {
    use std::os::unix::fs::PermissionsExt;

    let home = scratch("send-fetched-mode");
    let dir = config_dir(&home);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let (_, token_url) = answer_once("200 OK", r#"{"token":"pk.fetched"}"#);
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.fetched");
    cli_token_state(&home, "pk.fetched");
    let mode = std::fs::metadata(&dir)
        .expect("the config dir")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o755,
        "the write changed the directory's mode"
    );
}
