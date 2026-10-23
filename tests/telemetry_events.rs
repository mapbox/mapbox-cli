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
//! check behind `MAPBOX_INTERNAL_TOKENS_URL`, at loopback servers. Events are
//! never sent with the user's token, so any user token in these tests is
//! there to show it is not sent.

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
/// token to send with, since the token endpoint is not there.
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
        .env("MAPBOX_CLI_TOKEN", "pk.test")
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
        .env("MAPBOX_CLI_TOKEN", "pk.test")
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
        .env("MAPBOX_CLI_TOKEN", "pk.test")
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
        .env("MAPBOX_CLI_TOKEN", "pk.test")
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
        .env("MAPBOX_CLI_TOKEN", "pk.test")
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
        .env("MAPBOX_CLI_TOKEN", "pk.test")
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
        .env("MAPBOX_CLI_TOKEN", "pk.test")
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
fn a_users_token_never_reaches_the_send() {
    for args in USER_TOKEN_RUNS {
        let home = scratch("send-never-user");
        log_in(&home, "tk.login");
        let (received, url) = events_server(true);
        let _ = command(&home)
            .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
            .env("MAPBOX_ACCESS_TOKEN", "sk.env")
            .env("MapboxAccessToken", "sk.legacy")
            .env("MAPBOX_CLI_TOKEN", "pk.cli")
            .args(args)
            .output()
            .expect("run mapbox");
        let request = received
            .recv_timeout(Duration::from_secs(10))
            .expect("the event was posted");
        assert!(
            request.head.contains("access_token=pk.cli "),
            "{args:?}: {}",
            request.head
        );
        assert_eq!(referer(&request.head).as_deref(), CLI_REFERER, "{args:?}");
        let whole = format!("{}{}", request.head, request.body);
        for user in ["sk.typed", "tk.login", "sk.env", "sk.legacy"] {
            assert!(!whole.contains(user), "{args:?}: `{user}` was sent");
        }
    }
}

/// An overridden URL still gets the CLI's token and its `Referer`.
#[test]
fn an_overridden_url_keeps_the_clis_token() {
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
    assert_eq!(
        referer(&request.head).as_deref(),
        Some("https://cli.mapbox.com/"),
        "the CLI's token is restricted to that URL"
    );
}

/// The `Referer` header a request carried, if any.
fn referer(head: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("referer")
            .then(|| value.trim().to_string())
    })
}

/// Saves a login holding `token`. It has no `exp`, so it never needs a
/// refresh.
fn log_in(home: &Path, token: &str) {
    std::fs::write(
        config_dir(home).join("credentials.json"),
        format!(r#"{{"access_token":"{token}"}}"#),
    )
    .expect("write a login");
}

/// Tokens whose payloads claim the accounts in their names.
const LOGIN_TOKEN: &str = "tk.eyJ1IjoibG9naW4tdXNlciIsImEiOiJ4In0.SIG";
const ENV_TOKEN: &str = "pk.eyJ1IjoiZW52LXVzZXIiLCJhIjoieCJ9.SIG";
const WORK_TOKEN: &str = "sk.eyJ1Ijoid29yay11c2VyIiwiYSI6IngifQ.SIG";

/// `auth` as a run's event reported it.
fn auth_of(cmd: &mut Command) -> Value {
    sent(cmd).1["auth"].clone()
}

fn auth(source: &str, kind: &str, account: &str) -> Value {
    serde_json::json!({ "source": source, "type": kind, "account": account })
}

/// A run that sends no request names the token a service command would
/// have sent, chosen in the same order, so every event of a logged-in user
/// names their account.
#[test]
fn a_run_without_a_request_names_the_token_an_api_call_would_use() {
    let home = scratch("would-use-token");
    log_in(&home, LOGIN_TOKEN);
    std::fs::write(
        config_dir(&home).join("credentials-work.json"),
        format!(r#"{{"access_token":"{WORK_TOKEN}"}}"#),
    )
    .expect("write a second login");

    let login = auth("login", "tk", "login-user");
    let env = auth("environment", "pk", "env-user");
    let cases: [(&[&str], Option<&str>, Value); 7] = [
        (&["config", "list"], None, login.clone()),
        (&["--version"], None, login.clone()),
        (&["styles", "--help"], None, login.clone()),
        // The environment outranks a login, as it does for an API call…
        (&["config", "list"], Some(ENV_TOKEN), env.clone()),
        (&["--version"], Some(ENV_TOKEN), env),
        // …unless `--use-login` skips it.
        (&["config", "list", "--use-login"], Some(ENV_TOKEN), login),
        (
            &["config", "list", "--profile", "work"],
            None,
            auth("login", "sk", "work-user"),
        ),
    ];
    for (args, environment, expected) in cases {
        let mut cmd = command(&home);
        cmd.args(args);
        if let Some(token) = environment {
            cmd.env("MAPBOX_ACCESS_TOKEN", token);
        }
        assert_eq!(auth_of(&mut cmd), expected, "{args:?} {environment:?}");
    }
}

/// `report` goes with the CLI's own token when it can; a login standing in
/// for it would say the run sent the user's.
#[test]
fn a_run_sent_with_the_clis_token_reports_no_auth() {
    let home = scratch("would-use-cli-token");
    log_in(&home, LOGIN_TOKEN);
    let (out, event) = run_sent(&home, &["-o", "json", "report", "--dry-run", "A test"]);
    let plan: Value = serde_json::from_slice(&out.stdout).expect("a JSON plan");
    assert_eq!(
        plan["token"], "cli",
        "the run should have used the CLI's token"
    );
    assert_eq!(event["auth"], Value::Null);
}

#[test]
fn no_token_anywhere_reports_no_auth() {
    let home = scratch("would-use-none");
    for args in [&["config", "list"][..], &["--version"]] {
        assert_eq!(auth_of(command(&home).args(args)), Value::Null, "{args:?}");
    }
    // A profile that was never logged in, as an API call would find too.
    log_in(&home, LOGIN_TOKEN);
    assert_eq!(
        auth_of(command(&home).args(["config", "list", "--profile", "work"])),
        Value::Null
    );
}

/// Help clap answers itself is not parsed, so a typed `--token` or
/// `--profile` can't be read; guessing past it could name the wrong account.
#[test]
fn an_unparsed_run_that_chooses_its_token_reports_no_auth() {
    let home = scratch("would-use-unparsed");
    log_in(&home, LOGIN_TOKEN);
    for args in [
        &["styles", "--help", "--token", ENV_TOKEN][..],
        &["styles", "--help", "-t", ENV_TOKEN],
        &["--profile=work", "--version"],
    ] {
        assert_eq!(auth_of(command(&home).args(args)), Value::Null, "{args:?}");
    }
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

/// Telemetry reads the saved login only for its account, never through the
/// lock, so sending creates no lock file and doesn't change the directory's
/// permissions.
#[cfg(unix)]
#[test]
fn the_send_leaves_a_login_and_its_directory_alone() {
    use std::os::unix::fs::PermissionsExt;

    let home = scratch("send-login-readonly");
    let dir = config_dir(&home);
    log_in(&home, "tk.login");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_CLI_TOKEN", "pk.cli")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.cli");

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

/// A token called invalid for something fixed in the Console stays the same
/// string, so the dead token served again is checked once more, and taken
/// back once it checks valid.
#[test]
fn the_rejected_token_served_again_is_taken_back_once_valid() {
    let home = scratch("send-taken-back");
    std::fs::write(
        config_dir(&home).join("cli-token.json"),
        r#"{"token":"pk.fixed","rejected":true,"overridden":true}"#,
    )
    .expect("write the state");
    let (_, token_url) = answer_once("200 OK", r#"{"token":"pk.fixed"}"#);
    let (checked, tokens_url) = answer_once("200 OK", r#"{"code":"TokenValid"}"#);
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .env("MAPBOX_INTERNAL_TOKENS_URL", &tokens_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    checked
        .recv_timeout(Duration::from_secs(10))
        .expect("the token was checked");
    assert_eq!(sent_with(&received), "pk.fixed");
    assert_eq!(cli_token_state(&home, "pk.fixed")["rejected"], false);
}

/// `config set telemetry off` covers the token fetch as
/// `MAPBOX_CLI_NO_TELEMETRY` does: telemetry is its only user.
#[test]
fn telemetry_off_fetches_no_token() {
    let home = scratch("send-off-no-fetch");
    let (asked, token_url) = answer_once("200 OK", r#"{"token":"pk.fetched"}"#);
    let (received, url) = events_server(true);
    for args in [
        &["config", "set", "telemetry", "off"][..],
        &["config", "list"],
    ] {
        let out = command(&home)
            .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
            .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
            .args(args)
            .output()
            .expect("run mapbox");
        assert!(out.status.success());
    }
    assert!(
        asked.recv_timeout(Duration::from_secs(3)).is_err(),
        "`telemetry off` still fetched the CLI's token"
    );
    assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
    assert!(!config_dir(&home).join("cli-token.json").exists());
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

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs()
}

/// Marks the state `overridden`, as a dev build pointed at loopback writes
/// it; otherwise the run would not trust it.
fn save_cli_token_state(home: &Path, state: Value) {
    let mut state = state;
    state["overridden"] = Value::Bool(true);
    std::fs::write(config_dir(home).join("cli-token.json"), state.to_string())
        .expect("write the state");
}

/// The `access_token` and `Referer` of the next request in `heads`.
fn token_sent(heads: &mpsc::Receiver<String>) -> (String, Option<String>) {
    let head = heads
        .recv_timeout(Duration::from_secs(10))
        .expect("an event was posted");
    let token = head
        .split("access_token=")
        .nth(1)
        .and_then(|rest| rest.split([' ', '&']).next())
        .unwrap_or_default()
        .to_string();
    (token, referer(&head))
}

const CLI_REFERER: Option<&str> = Some("https://cli.mapbox.com/");

/// Runs where the user has a token: passed with `--token`, from the login,
/// and commands that never resolve one.
const USER_TOKEN_RUNS: [&[&str]; 5] = [
    &["--token", "sk.typed", "config", "list"],
    &["--use-login", "config", "list"],
    &["--version"],
    &["nosuchcommand"],
    &["styles", "list", "--schema"],
];

#[test]
fn a_logged_in_run_sends_with_the_clis_token() {
    for args in USER_TOKEN_RUNS {
        let home = scratch("send-logged-in-cli");
        log_in(&home, "tk.login");
        let (_, token_url) = answer_once("200 OK", r#"{"token":"pk.fetched"}"#);
        let (received, url) = events_server(true);
        let _ = command(&home)
            .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
            .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
            // A `pk.` token passes the check a fetched token must, so it
            // would get through a fallback that `sk.` and `tk.` would not.
            .env("MAPBOX_ACCESS_TOKEN", "pk.env")
            .env("MapboxAccessToken", "sk.legacy")
            .args(args)
            .output()
            .expect("run mapbox");
        let request = received
            .recv_timeout(Duration::from_secs(10))
            .expect("the event was posted");
        assert!(
            request.head.contains("access_token=pk.fetched "),
            "{args:?}: {}",
            request.head
        );
        let whole = format!("{}{}", request.head, request.body);
        for user in ["sk.typed", "tk.login", "pk.env", "sk.legacy"] {
            assert!(!whole.contains(user), "{args:?}: `{user}` was sent");
        }
    }
}

/// Runs `config list` with a user token in the environment. Returns whether
/// an event arrived, and the delivery log.
fn try_sending(home: &Path, token_url: &str) -> (bool, Vec<Value>) {
    let (received, url) = events_server(true);
    let out = command(home)
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", token_url)
        .env("MAPBOX_ACCESS_TOKEN", "sk.env")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    // One line when the event is dropped at once, two when the child ran.
    let mut lines = deliveries(home, 1);
    if lines.last().is_some_and(|line| line["stage"] == "queued") {
        lines = deliveries(home, 2);
    }
    let arrived = received.recv_timeout(Duration::from_millis(500)).is_ok();
    (arrived, lines)
}

/// A `403` for a token the Tokens API calls valid. The event is dropped, not
/// resent with another token.
#[test]
fn a_refused_clis_token_is_not_sent_again() {
    let home = scratch("send-refused-once");
    log_in(&home, "tk.login");
    save_cli_token_state(&home, serde_json::json!({"token": "pk.kept"}));
    let (heads, events_url) = answer_each(&[
        ("403 Forbidden", r#"{"message":"Forbidden"}"#),
        ("204 No Content", ""),
    ]);
    let (_, tokens_url) = answer_once("200 OK", r#"{"code":"TokenValid"}"#);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &events_url)
        .env("MAPBOX_INTERNAL_TOKENS_URL", &tokens_url)
        .env("MAPBOX_ACCESS_TOKEN", "sk.env")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(
        token_sent(&heads),
        ("pk.kept".to_string(), CLI_REFERER.map(str::to_owned))
    );
    // The count is written when the child finishes, so a resend would show
    // up within the wait below.
    cli_token_state(&home, r#""count":1"#);
    assert!(
        heads.recv_timeout(Duration::from_secs(1)).is_err(),
        "the event was sent again"
    );
}

#[test]
fn a_revoked_token_is_replaced_for_the_next_run_without_a_resend() {
    let home = scratch("send-rotated-revoked");
    save_cli_token_state(&home, serde_json::json!({"token": "pk.old"}));
    let (heads, events_url) = answer_each(&[
        ("401 Unauthorized", r#"{"message":"Not Authorized"}"#),
        ("204 No Content", ""),
    ]);
    // The real Tokens API hasn't been seen to wrap its verdict in a `401`,
    // but if it does, the `code` still decides.
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
    assert_eq!(token_sent(&heads).0, "pk.old");
    assert_eq!(cli_token_state(&home, "pk.new")["rejected"], false);
    assert!(
        heads.recv_timeout(Duration::from_secs(1)).is_err(),
        "the event was sent again"
    );

    // The token endpoint answered only once, so this run must use the saved
    // token.
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.new");
}

/// An `sk.` or `tk.` override drops the event, and the fetched token is not
/// sent instead.
#[test]
fn an_override_that_is_not_public_sends_nothing() {
    for token in ["sk.x", "tk.x"] {
        let home = scratch(&format!("send-override-{token}"));
        let (asked, token_url) = answer_once("200 OK", r#"{"token":"pk.fetched"}"#);
        let (received, url) = events_server(true);
        let out = command(&home)
            .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
            .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
            .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
            .env("MAPBOX_CLI_TOKEN", token)
            .args(["config", "list"])
            .output()
            .expect("run mapbox");
        assert!(out.status.success());
        let lines = deliveries(&home, 1);
        let last = lines.last().expect("a delivery line");
        assert_eq!(last["stage"], "no_token", "{token}: {lines:?}");
        assert_eq!(last["reason"], "override_not_public", "{token}");
        assert!(
            received.recv_timeout(Duration::from_millis(500)).is_err(),
            "{token}: sent"
        );
        assert!(
            asked.recv_timeout(Duration::from_millis(100)).is_err(),
            "{token}: fetched a token instead"
        );
    }
}

/// Even when the override matches the saved token, its refusal leaves the
/// saved state alone.
#[test]
fn a_refused_override_is_not_checked_or_replaced() {
    let home = scratch("send-override-refused");
    let state = serde_json::json!({"token": "pk.cli", "overridden": true}).to_string();
    std::fs::write(config_dir(&home).join("cli-token.json"), &state).expect("write the state");
    let (heads, events_url) = answer_each(&[
        ("401 Unauthorized", r#"{"message":"Not Authorized"}"#),
        ("204 No Content", ""),
    ]);
    let (fetched, token_url) = answer_once("200 OK", r#"{"token":"pk.new"}"#);
    let (checked, tokens_url) = answer_once("200 OK", r#"{"code":"TokenRevoked"}"#);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &events_url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .env("MAPBOX_INTERNAL_TOKENS_URL", &tokens_url)
        .env("MAPBOX_CLI_TOKEN", "pk.cli")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(token_sent(&heads).0, "pk.cli");
    // The child logs once it is done with the refusal.
    let lines = deliveries(&home, 2);
    assert_eq!(lines[1]["status"], 401, "{lines:?}");
    assert!(
        heads.recv_timeout(Duration::from_millis(500)).is_err(),
        "the event was sent again"
    );
    assert!(
        checked.recv_timeout(Duration::from_millis(100)).is_err(),
        "the override was checked"
    );
    assert!(
        fetched.recv_timeout(Duration::from_millis(100)).is_err(),
        "a token was fetched to replace the override"
    );
    let after =
        std::fs::read_to_string(config_dir(&home).join("cli-token.json")).expect("the state");
    assert_eq!(after, state);
}

/// A port nothing listens on, for an endpoint a run must not reach.
const NOWHERE: &str = "http://127.0.0.1:9/";

const NOT_AUTHORIZED: (&str, &str) = ("401 Unauthorized", r#"{"message":"Not Authorized"}"#);

/// Runs `config list` with the delivery log on, its event sent to `events`,
/// the token endpoint at `token` and the Tokens API at `tokens`.
fn run_against(home: &Path, events: &str, token: &str, tokens: &str) {
    let out = command(home)
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", events)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", token)
        .env("MAPBOX_INTERNAL_TOKENS_URL", tokens)
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
}

fn read_cli_token_state(home: &Path) -> Value {
    let text = std::fs::read_to_string(config_dir(home).join("cli-token.json")).expect("the state");
    serde_json::from_str(&text).expect("the state is JSON")
}

/// After a valid token is refused, the endpoint is asked once. The new token
/// it serves is used next run and resets telemetry's count. The refused
/// event is not resent.
#[test]
fn a_different_token_served_after_a_refusal_is_used_next_run() {
    let home = scratch("send-refused-rotated");
    save_cli_token_state(
        &home,
        serde_json::json!({"token": "pk.old",
            "callers": {"telemetry": {"count": 2, "token": "pk.old"}}}),
    );
    let (heads, events_url) = answer_each(&[NOT_AUTHORIZED, ("204 No Content", "")]);
    let (_, tokens_url) = answer_once("200 OK", r#"{"code":"TokenValid"}"#);
    let (fetches, token_url) = answer_each(&[
        ("200 OK", r#"{"token":"pk.new"}"#),
        ("200 OK", r#"{"token":"pk.new"}"#),
    ]);
    run_against(&home, &events_url, &token_url, &tokens_url);
    assert_eq!(token_sent(&heads).0, "pk.old");
    let state = cli_token_state(&home, "pk.new");
    assert!(state.get("callers").is_none(), "{state}");
    fetches
        .recv_timeout(Duration::from_secs(10))
        .expect("the endpoint was asked after the refusal");
    // The child logs once it is done.
    deliveries(&home, 2);
    assert!(
        fetches.recv_timeout(Duration::from_millis(500)).is_err(),
        "the endpoint was asked twice"
    );
    assert!(
        heads.recv_timeout(Duration::from_millis(100)).is_err(),
        "the event was sent again"
    );

    let (received, url) = events_server(true);
    run_against(&home, &url, &token_url, NOWHERE);
    assert_eq!(sent_with(&received), "pk.new");
    assert!(
        fetches.recv_timeout(Duration::from_millis(500)).is_err(),
        "fetched with a token on disk"
    );
}

/// While quiet, nothing is sent, checked or fetched, and the parent drops
/// the event without spawning a child. After the hour, a successful send
/// resets the count.
#[test]
fn the_same_token_after_three_refusals_quiets_telemetry_for_an_hour() {
    let home = scratch("send-refused-quiet");
    save_cli_token_state(&home, serde_json::json!({"token": "pk.kept"}));
    for n in 1..=3 {
        let (heads, events_url) = answer_each(&[NOT_AUTHORIZED]);
        let (_, tokens_url) = answer_once("200 OK", r#"{"code":"TokenValid"}"#);
        let (fetched, token_url) = answer_once("200 OK", r#"{"token":"pk.kept"}"#);
        run_against(&home, &events_url, &token_url, &tokens_url);
        assert_eq!(token_sent(&heads).0, "pk.kept", "run {n}");
        fetched
            .recv_timeout(Duration::from_secs(10))
            .expect("the endpoint was asked after the refusal");
        let state = cli_token_state(&home, &format!(r#""count":{n}"#));
        assert_eq!(state["token"], "pk.kept", "{state}");
        assert_eq!(state["rejected"], false, "{state}");
        deliveries(&home, 2 * n);
    }
    let mut state = read_cli_token_state(&home);
    let until = state["callers"]["telemetry"]["quiet_until"]
        .as_u64()
        .expect("a quiet period");
    assert!(until > now() + 3500, "{state}");

    let (sent, events_url) = answer_once(NOT_AUTHORIZED.0, NOT_AUTHORIZED.1);
    let (checked, tokens_url) = answer_once("200 OK", r#"{"code":"TokenValid"}"#);
    let (fetched, token_url) = answer_once("200 OK", r#"{"token":"pk.new"}"#);
    run_against(&home, &events_url, &token_url, &tokens_url);
    let lines = deliveries(&home, 7);
    assert_eq!(lines[6]["stage"], "no_token", "{lines:?}");
    assert_eq!(lines[6]["reason"], "quiet", "{lines:?}");
    assert!(
        sent.recv_timeout(Duration::from_millis(500)).is_err(),
        "sent while quiet"
    );
    assert!(checked.recv_timeout(Duration::from_millis(100)).is_err());
    assert!(
        fetched.recv_timeout(Duration::from_millis(100)).is_err(),
        "fetched while quiet"
    );

    state["callers"]["telemetry"]["quiet_until"] = (now() - 1).into();
    save_cli_token_state(&home, state);
    let (received, url) = events_server(true);
    run_against(&home, &url, NOWHERE, NOWHERE);
    assert_eq!(sent_with(&received), "pk.kept");
    deliveries(&home, 9);
    let state = read_cli_token_state(&home);
    assert!(state.get("callers").is_none(), "{state}");
}

/// After the quiet hour, a new token from the endpoint resets the count.
#[test]
fn fetches_with_no_usable_token_quiet_telemetry() {
    let home = scratch("send-fetch-quiet");
    save_cli_token_state(&home, serde_json::json!({}));
    for n in 1..=3 {
        let (fetched, token_url) = answer_once("200 OK", r#"{"token":"sk.not-public"}"#);
        run_against(&home, NOWHERE, &token_url, NOWHERE);
        fetched
            .recv_timeout(Duration::from_secs(10))
            .expect("the endpoint was asked");
        cli_token_state(&home, &format!(r#""count":{n}"#));
        deliveries(&home, 2 * n);
    }

    let (fetched, token_url) = answer_once("200 OK", r#"{"token":"pk.new"}"#);
    run_against(&home, NOWHERE, &token_url, NOWHERE);
    let lines = deliveries(&home, 7);
    assert_eq!(lines[6]["reason"], "quiet", "{lines:?}");
    assert!(
        fetched.recv_timeout(Duration::from_millis(500)).is_err(),
        "fetched while quiet"
    );

    let mut state = read_cli_token_state(&home);
    state["callers"]["telemetry"]["quiet_until"] = 1.into();
    save_cli_token_state(&home, state);
    let (received, url) = events_server(true);
    run_against(&home, &url, &token_url, NOWHERE);
    assert_eq!(sent_with(&received), "pk.new");
    let state = cli_token_state(&home, "pk.new");
    assert!(state.get("callers").is_none(), "{state}");
}

/// A `500` says nothing about whether the API accepts the token, so it
/// doesn't reset the count.
#[test]
fn only_an_accepted_event_ends_the_count() {
    let home = scratch("send-count-ends");
    save_cli_token_state(
        &home,
        serde_json::json!({"token": "pk.kept",
            "callers": {"telemetry": {"count": 2, "token": "pk.kept"}}}),
    );
    let (heads, events_url) = answer_each(&[("500 Internal Server Error", "{}")]);
    run_against(&home, &events_url, NOWHERE, NOWHERE);
    assert_eq!(token_sent(&heads).0, "pk.kept");
    let lines = deliveries(&home, 2);
    assert_eq!(lines[1]["status"], 500, "{lines:?}");
    let state = read_cli_token_state(&home);
    assert_eq!(state["callers"]["telemetry"]["count"], 2, "{state}");

    let (received, url) = events_server(true);
    run_against(&home, &url, NOWHERE, NOWHERE);
    assert_eq!(sent_with(&received), "pk.kept");
    deliveries(&home, 4);
    let state = read_cli_token_state(&home);
    assert!(state.get("callers").is_none(), "{state}");
}

/// A refusal the Tokens API can't explain counts as a failure, and so does
/// a fetch after it with no response. Otherwise every run would send the
/// same token, likely be refused, and pay for another check.
#[test]
fn an_unexplained_refusal_counts_too() {
    let home = scratch("send-unknown");
    save_cli_token_state(&home, serde_json::json!({"token": "pk.kept"}));
    let (_, events_url) = answer_each(&[("403 Forbidden", r#"{"message":"Forbidden"}"#)]);
    let (checked, tokens_url) = answer_once("500 Internal Server Error", "{}");
    run_against(&home, &events_url, NOWHERE, &tokens_url);
    checked
        .recv_timeout(Duration::from_secs(10))
        .expect("the token was checked");
    let state = cli_token_state(&home, r#""count":1"#);
    assert_eq!(state["token"], "pk.kept");
    assert_eq!(state["rejected"], false);
}

/// Two cases: no saved token and a dead endpoint, which only the child finds
/// out; and telemetry quiet with a good saved token, which the parent knows
/// before spawning anything.
#[test]
fn an_unusable_clis_token_sends_nothing_even_with_a_users_token() {
    let later = now() + 3000;
    for (name, state) in [
        ("endpoint-dead", None),
        (
            "quiet",
            Some(serde_json::json!({"token": "pk.kept",
                "callers": {"telemetry": {"count": 3, "quiet_until": later,
                "token": "pk.kept"}}})),
        ),
    ] {
        let home = scratch(&format!("send-nothing-{name}"));
        log_in(&home, "tk.login");
        let quiet = state.is_some();
        if let Some(state) = state {
            save_cli_token_state(&home, state);
        }
        let (arrived, lines) = try_sending(&home, NOWHERE);
        assert!(!arrived, "{name}: sent");
        let last = lines.last().expect("a delivery line");
        if !quiet {
            assert_eq!(last["stage"], "failed", "{name}: {lines:?}");
            assert_eq!(last["error"], "no token to send with", "{name}");
        } else {
            assert_eq!(last["stage"], "no_token", "{name}: {lines:?}");
            assert_eq!(last["reason"], "quiet", "{name}: {lines:?}");
        }
    }
}

/// Another caller's quiet period doesn't hold telemetry back, and neither do
/// the waits an earlier build wrote: an upgrade must not leave anyone
/// waiting.
#[test]
fn no_wait_but_telemetrys_own_holds_it() {
    let later = now() + 3000;
    for (name, state, expected) in [
        (
            "other-caller",
            serde_json::json!({"token": "pk.kept", "callers": {"other": {"count": 3,
                "quiet_until": later, "token": "pk.kept"}}}),
            "pk.kept",
        ),
        (
            "earlier-build-pause",
            serde_json::json!({"token": "pk.old", "rejected": true,
                "failures": 3, "retry_at": later}),
            "pk.new",
        ),
        (
            "earlier-build-rest",
            serde_json::json!({"token": "pk.kept", "failures": 3, "retry_at": later,
                "cooldowns": {"telemetry": {"failures": 3, "until": later,
                "misses": 3, "retry_at": later}}}),
            "pk.kept",
        ),
    ] {
        let home = scratch(&format!("send-not-held-{name}"));
        save_cli_token_state(&home, state);
        let (_, token_url) = answer_once("200 OK", r#"{"token":"pk.new"}"#);
        let (received, url) = events_server(true);
        run_against(&home, &url, &token_url, NOWHERE);
        assert_eq!(sent_with(&received), expected, "{name}");
        if name == "other-caller" {
            deliveries(&home, 2);
            let state = read_cli_token_state(&home);
            assert_eq!(state["callers"]["other"]["count"], 3, "{state}");
            assert!(state["callers"].get("telemetry").is_none(), "{state}");
        }
    }
}

/// The quiet period applies only to the fetched token.
#[test]
fn an_override_is_sent_while_telemetry_is_quiet() {
    let home = scratch("send-override-quiet");
    save_cli_token_state(
        &home,
        serde_json::json!({"token": "pk.kept",
            "callers": {"telemetry": {"count": 3, "quiet_until": now() + 3000,
            "token": "pk.kept"}}}),
    );
    let (received, url) = events_server(true);
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_CLI_TOKEN", "pk.cli")
        .args(["config", "list"])
        .output()
        .expect("run mapbox");
    assert!(out.status.success());
    assert_eq!(sent_with(&received), "pk.cli");
}

/// A new token resets every caller: telemetry fetching one ends another
/// caller's quiet period, which was set with the old token.
#[test]
fn a_new_token_telemetry_fetches_ends_another_callers_quiet_period() {
    let home = scratch("send-new-token-resets-others");
    save_cli_token_state(
        &home,
        serde_json::json!({"token": "pk.old", "rejected": true, "callers": {"other":
            {"count": 3, "quiet_until": now() + 3000, "token": "pk.old"}}}),
    );
    let (_, token_url) = answer_once("200 OK", r#"{"token":"pk.new"}"#);
    let (received, url) = events_server(true);
    run_against(&home, &url, &token_url, NOWHERE);
    assert_eq!(sent_with(&received), "pk.new");
    let state = cli_token_state(&home, "pk.new");
    assert!(state.get("callers").is_none(), "{state}");
}

/// `report` fetching a new token while telemetry is quiet ends telemetry's
/// quiet period, so the run's own event goes out with that token.
#[test]
fn a_new_token_report_fetches_ends_telemetrys_quiet_period() {
    let home = scratch("send-report-resets-telemetry");
    save_cli_token_state(
        &home,
        serde_json::json!({"token": "pk.dead", "rejected": true, "callers": {"telemetry":
            {"count": 3, "quiet_until": now() + 3000, "token": "pk.dead"}}}),
    );
    let (fetched, token_url) = answer_once("200 OK", r#"{"token":"pk.fresh"}"#);
    let (received, url) = events_server(true);
    // The report itself goes to a dead proxy, so nothing reaches Mapbox.
    let out = command(&home)
        .env("MAPBOX_INTERNAL_TELEMETRY_LOG", "1")
        .env("MAPBOX_INTERNAL_TELEMETRY_URL", &url)
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", &token_url)
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .args(["report", "A test"])
        .output()
        .expect("run mapbox");
    assert!(!out.status.success(), "the report reached somewhere");
    fetched
        .recv_timeout(Duration::from_secs(10))
        .expect("report fetched a token");
    assert_eq!(sent_with(&received), "pk.fresh");
    let state = cli_token_state(&home, "pk.fresh");
    assert!(state.get("callers").is_none(), "{state}");
}

/// A search query is whatever the caller typed — a customer's name, a
/// token pasted by mistake — so it leaves as its length and never as text.
#[test]
fn a_search_query_is_sent_as_its_length() {
    let home = scratch("commands-search");
    let (out, event) = run_sent(
        &home,
        &[
            "-o",
            "json",
            "commands",
            "search",
            "secret-customer-name",
            "upload",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(event["command"], serde_json::json!(["commands", "search"]));
    assert!(
        !event.to_string().contains("secret-customer-name"),
        "the query's text was sent: {event}"
    );
    assert_eq!(
        event["params"],
        serde_json::json!([{ "name": "query", "length": "secret-customer-name upload".len() }])
    );
}
