//! End-to-end tests for `--dry-run`.
//!
//! The flag's promise is a narrow one and worth pinning as a whole: on a
//! command that changes something, it prints what would be sent and changes
//! nothing. Both halves need a real process to check. The unit tests in
//! `src/executor.rs` cover the rendering as pure functions, and the ones in
//! `src/main.rs` pin which commands carry the flag; what only a child process
//! shows is which stream the plan lands on, what the exit code is, and that
//! the credential store on disk is the same afterwards.
//!
//! Nothing here reaches the network — that is most of the point — and nothing
//! touches the developer's own credentials: every test gets its own
//! `MAPBOX_CONFIG_DIR`, so the stores these write and read are their own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};

/// A credential store of this test's own.
///
/// `MAPBOX_CONFIG_DIR` rather than a redirected `HOME`: it names the store
/// directly, so a test that asserts a file was left alone is pointing at the
/// same path the CLI resolved. One directory per test keeps the suite
/// parallel — `cargo test` runs these concurrently, and a shared store would
/// make "the file is still there" depend on test order.
fn config_dir(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("dry-run-{test}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create config dir");
    dir
}

/// Runs the real binary with the developer's own environment cleared.
///
/// A `MAPBOX_ACCESS_TOKEN` in the running shell would silently supply the
/// token these tests are asserting about, and `MAPBOX_OUTPUT` would change
/// the shape of every assertion.
fn command(config: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    // Telemetry is not under test here, and would post to Mapbox.
    cmd.env("MAPBOX_CLI_NO_TELEMETRY", "1")
        .env_remove("MAPBOX_CLI_TOKEN")
        .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", "http://127.0.0.1:9/")
        .env("MAPBOX_INTERNAL_TOKENS_URL", "http://127.0.0.1:9/");
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_DEBUG")
        .env("MAPBOX_CONFIG_DIR", config);
    cmd
}

fn run(config: &Path, args: &[&str]) -> Output {
    command(config).args(args).output().expect("run mapbox")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("expected JSON, got {text:?} ({e})"))
}

/// A token shaped like a real one, carrying whatever claims are asked for.
///
/// `exp` is the one that matters here: a token without it is never considered
/// stale, so nothing tries to refresh it and nothing reaches the network.
fn fake_token(claims: &str) -> String {
    let payload = STANDARD_NO_PAD
        .encode(claims)
        .replace('+', "-")
        .replace('/', "_");
    format!("tk.{payload}.signature")
}

/// Puts a stored login in the config dir, as `mapbox auth login` would have.
fn write_login(config: &Path, access_token: &str) -> PathBuf {
    let path = config.join("credentials.json");
    let credentials = format!(
        r#"{{"access_token":"{access_token}","refresh_token":"a-refresh-token",
            "username":"zz-clitest","client_id":"a-client-id"}}"#
    );
    std::fs::write(&path, credentials).expect("write credentials");
    path
}

/// The whole contract in one command: the request is described, the exit code
/// is a success, and nothing about it says a call was made.
#[test]
fn a_mutating_command_describes_the_request_it_would_send() {
    let config = config_dir("describes");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "delete",
            "zz-clitest-style",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let plan = json(&stdout(&out));
    assert_eq!(plan["dry_run"], true);
    assert_eq!(plan["command"], "styles delete");
    assert_eq!(plan["method"], "DELETE");
    assert_eq!(
        plan["url"],
        "https://api.mapbox.com/styles/v1/zz-clitest/zz-clitest-style"
    );
    // No body was given and none is invented.
    assert_eq!(plan["body"], serde_json::Value::Null);
}

/// The plan is the request, and the request carries the token in its query
/// string — so the one output whose whole purpose is to be read, pasted and
/// captured is also the one most able to leak a live credential.
#[test]
fn the_plan_never_carries_the_access_token() {
    let config = config_dir("redaction");
    let secret = "sk.zz-clitest-not-a-real-token";
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--debug",
            "--username",
            "zz-clitest",
            "--token",
            secret,
            "styles",
            "create",
            "--data",
            r#"{"name":"zz-clitest"}"#,
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let out_text = stdout(&out);
    let err_text = stderr(&out);
    assert!(!out_text.contains(secret), "token on stdout: {out_text}");
    assert!(!err_text.contains(secret), "token on stderr: {err_text}");
    assert_eq!(json(&out_text)["query"]["access_token"], "<redacted>");
}

/// A body reaches the plan as the document it parses to, not as the string it
/// was typed as — the one thing worth checking before a `create` runs for
/// real is what the API will receive.
#[test]
fn the_plan_shows_the_body_that_would_be_sent() {
    let config = config_dir("body");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "create",
            "--data",
            r#"{"name":"zz-clitest","version":8}"#,
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let body = &json(&stdout(&out))["body"];
    assert_eq!(body["source"], "--data");
    assert_eq!(body["content_type"], "application/json");
    assert_eq!(body["json"]["name"], "zz-clitest");
    assert_eq!(body["json"]["version"], 8);
}

/// A negative `--lon` is the case a flag parser gets wrong.
#[test]
fn field_flags_build_the_body_over_data() {
    let config = config_dir("body-fields");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--token",
            "sk.a-test-token",
            "feedback",
            "create",
            "--data",
            r#"{"id":"77bf502a-026a-460b-a3d1-56a604465a7d","category":"from-data","lat":1}"#,
            "--feedback",
            "A test",
            "--lat",
            "37.77",
            "--lon",
            "-122.4",
            "--category",
            "poi",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let body = &json(&stdout(&out))["body"]["json"];
    assert_eq!(
        body,
        &serde_json::json!({
            "id": "77bf502a-026a-460b-a3d1-56a604465a7d",
            "feedback": "A test",
            "lat": 37.77,
            "lon": -122.4,
            "category": "poi",
        })
    );
}

/// A default that won over `--data` would silently move a real place to 0,0.
#[test]
fn coordinates_default_to_zero_only_when_unset() {
    let config = config_dir("body-field-defaults");
    let body = |extra: &[&str]| {
        let mut args = vec![
            "-o",
            "json",
            "--token",
            "sk.a-test-token",
            "feedback",
            "create",
            "--feedback",
            "A test",
            "--category",
            "cli",
            "--dry-run",
        ];
        args.extend_from_slice(extra);
        let out = run(&config, &args);
        assert!(out.status.success(), "{}", stderr(&out));
        json(&stdout(&out))["body"]["json"].clone()
    };

    let unset = body(&[]);
    assert_eq!((&unset["lat"], &unset["lon"]), (&0.into(), &0.into()));

    let from_data = body(&["--data", r#"{"lat":60.17,"lon":24.94}"#]);
    assert_eq!(from_data["lat"], 60.17);
    assert_eq!(from_data["lon"], 24.94);
}

/// What the flag is for. A dry run that accepted a body the real call would
/// reject would be worse than no dry run at all — it would be a green light
/// on the mistake it was asked to look for.
#[test]
fn a_dry_run_rejects_a_body_the_real_call_would_reject() {
    let config = config_dir("invalid-body");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "create",
            "--data",
            "{not json",
            "--dry-run",
        ],
    );

    assert!(!out.status.success());
    // A failure is not a result: stdout stays empty in every mode.
    assert_eq!(stdout(&out), "");
    assert_eq!(json(&stderr(&out))["code"], "invalid_data");
}

/// Offered only where it means something. A `--dry-run` on a `GET` would be a
/// flag that reads as a safety net and does nothing, so it is not a flag.
#[test]
fn a_read_only_command_does_not_take_dry_run() {
    let config = config_dir("read-only");
    let out = run(
        &config,
        &[
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "list",
            "--dry-run",
        ],
    );

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("--dry-run"),
        "the error should name the argument: {}",
        stderr(&out)
    );
}

/// `auth logout` deletes a file, and which file depends on `--profile` and
/// `MAPBOX_CONFIG_DIR` — exactly the sort of thing worth being shown before
/// it happens rather than after.
#[test]
fn logout_names_the_file_it_would_delete_and_leaves_it_there() {
    let config = config_dir("logout");
    let path = write_login(&config, &fake_token(r#"{"u":"zz-clitest"}"#));

    let out = run(&config, &["-o", "json", "auth", "logout", "--dry-run"]);

    assert!(out.status.success(), "{}", stderr(&out));
    let plan = json(&stdout(&out));
    assert_eq!(plan["dry_run"], true);
    assert_eq!(plan["command"], "auth logout");
    assert_eq!(plan["would_delete"], true);
    assert_eq!(plan["credentials_path"], path.display().to_string());
    assert!(path.exists(), "the dry run deleted the credentials");
}

/// Nothing stored is not a failure — `auth logout` succeeds there too — but
/// the plan has to say so, or a reader takes "would delete" as read.
#[test]
fn logout_says_when_there_is_nothing_to_delete() {
    let config = config_dir("logout-empty");
    let out = run(&config, &["-o", "json", "auth", "logout", "--dry-run"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(json(&stdout(&out))["would_delete"], false);
}

/// A precondition the real command would fail on is a failure here too.
/// Describing a refresh that cannot run would be the same false green light
/// as accepting a body that cannot be sent.
#[test]
fn refresh_fails_the_dry_run_when_there_is_nothing_to_refresh() {
    let config = config_dir("refresh-empty");
    let out = run(&config, &["-o", "json", "auth", "refresh", "--dry-run"]);

    assert!(!out.status.success());
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("Not currently logged in"),
        "{}",
        stderr(&out)
    );
}

/// The subtlest thing a dry run could get wrong.
///
/// Resolving a token normally goes through `load_fresh_credentials`, which
/// spends the stored refresh token and rewrites the credentials file when the
/// access token has expired. That is a mutation, on the one code path that
/// promised not to make any — and it happens before the command being
/// previewed is even reached.
///
/// A stored token with an `exp` in the past is what triggers it. The
/// assertion is on `--debug`'s own line, printed before the request goes out,
/// so a regression fails here rather than depending on whether the network
/// was reachable.
#[test]
fn a_dry_run_does_not_spend_the_stored_refresh_token() {
    let config = config_dir("no-refresh");
    let expired = fake_token(r#"{"u":"zz-clitest","exp":1000000000}"#);
    let path = write_login(&config, &expired);
    let before = std::fs::read_to_string(&path).expect("read credentials");

    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--debug",
            "styles",
            "delete",
            "zz-clitest-style",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !stderr(&out).contains("grant_type=refresh_token"),
        "the dry run tried to refresh: {}",
        stderr(&out)
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read credentials"),
        before,
        "the dry run rewrote the credentials file",
    );
}

/// Under `text` the plan is still the result, so it is still stdout — a
/// `--dry-run > plan.txt` has to catch the request, not an empty file.
#[test]
fn the_plan_is_a_result_and_goes_to_stdout() {
    let config = config_dir("text-mode");
    let out = run(
        &config,
        &[
            "-o",
            "text",
            "--username",
            "zz-clitest",
            "--token",
            "sk.a-test-token",
            "styles",
            "delete",
            "zz-clitest-style",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Dry run"), "{text}");
    assert!(
        text.contains("DELETE https://api.mapbox.com/styles/v1/zz-clitest/zz-clitest-style"),
        "{text}"
    );
}

/// `places` always sends the batch request, one id or many — the whole
/// point of merging `places get`/`places batch` into one command.
#[test]
fn places_always_batches_even_for_one_id() {
    let config = config_dir("places-one-id");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--token",
            "sk.a-test-token",
            "places",
            "zz-mapbox-id-1",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let body = &json(&stdout(&out))["body"]["json"];
    assert_eq!(body, &serde_json::json!({ "ids": ["zz-mapbox-id-1"] }));
}

#[test]
fn places_takes_more_than_one_id() {
    let config = config_dir("places-many-ids");
    let out = run(
        &config,
        &[
            "-o",
            "json",
            "--token",
            "sk.a-test-token",
            "places",
            "zz-mapbox-id-1",
            "zz-mapbox-id-2",
            "zz-mapbox-id-3",
            "--dry-run",
        ],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    let body = &json(&stdout(&out))["body"]["json"];
    assert_eq!(
        body,
        &serde_json::json!({ "ids": ["zz-mapbox-id-1", "zz-mapbox-id-2", "zz-mapbox-id-3"] })
    );
}

/// The API's own limit (100), enforced before the request goes out rather
/// than left to a 400 that says the same thing less directly.
#[test]
fn places_refuses_more_than_a_hundred_ids_before_sending() {
    let config = config_dir("places-too-many-ids");
    let ids: Vec<String> = (0..101).map(|n| format!("zz-id-{n}")).collect();
    let mut args = vec!["-o", "json", "--token", "sk.a-test-token", "places"];
    args.extend(ids.iter().map(String::as_str));
    args.push("--dry-run");

    let out = run(&config, &args);

    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("no more were expected"),
        "{}",
        stderr(&out)
    );
}

/// `report`'s message is its positional, `lat`/`lon` are sent without being
/// typed, and it goes to the CLI endpoint, not the one `feedback create`
/// files under the caller's account.
#[test]
fn report_sends_the_message_to_the_cli_endpoint() {
    let config = config_dir("report");
    let plan = |extra: &[&str]| {
        let mut args = vec![
            "-o",
            "json",
            "--token",
            "sk.a-test-token",
            "report",
            "--dry-run",
            "A test",
        ];
        args.extend_from_slice(extra);
        let out = run(&config, &args);
        assert!(out.status.success(), "{}", stderr(&out));
        json(&stdout(&out))
    };

    let defaults = plan(&[]);
    assert_eq!(
        defaults["url"],
        "https://api.mapbox.com/user-feedback/v1/cli-feedback"
    );
    assert_eq!(
        defaults["body"]["json"],
        serde_json::json!({ "feedback": "A test", "lat": 0, "lon": 0, "category": "general" })
    );

    let categorized = plan(&["--category", "bug"]);
    assert_eq!(categorized["body"]["json"]["category"], "bug");
}

/// The positional is the whole body; a `--data` would be a second way to
/// say it, or a way to send a place this endpoint has no use for.
#[test]
fn report_needs_a_message_and_takes_no_data() {
    let config = config_dir("report-usage");
    for args in [
        &["--token", "sk.a-test-token", "report", "--dry-run"][..],
        &[
            "--token",
            "sk.a-test-token",
            "report",
            "A test",
            "--data",
            "{}",
            "--dry-run",
        ][..],
    ] {
        let out = run(&config, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).is_empty(), "{args:?}: {}", stdout(&out));
    }
}

/// What `report` sends with, and what it says, when it cannot use the CLI's
/// token. Every case here is decided before a request: the token endpoint
/// is a dead port, and a cooldown is written to disk.
mod report_token {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_secs()
    }

    fn write_state(config: &Path, state: &str) {
        std::fs::write(config.join("cli-token.json"), state).expect("write the state");
    }

    fn error_code(out: &Output) -> String {
        json(&stderr(out))["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// A token endpoint on loopback that hands out `token` once.
    fn token_endpoint(token: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let body = format!(r#"{{"token":"{token}"}}"#);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        format!("http://{addr}/config/token.json")
    }

    /// A request that goes nowhere: the proxy is a dead port, so `--debug`
    /// shows what was about to be sent without anything reaching Mapbox.
    fn sent_nowhere(config: &Path, args: &[&str]) -> Command {
        let mut cmd = command(config);
        cmd.env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .args(args);
        cmd
    }

    #[test]
    fn with_no_token_at_all_it_says_the_clis_is_unavailable() {
        let config = config_dir("report-no-tokens");
        let out = run(&config, &["-o", "json", "report", "A test"]);
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        assert!(stdout(&out).is_empty(), "{}", stdout(&out));
        assert_eq!(error_code(&out), "cli_token_unavailable");
    }

    /// The CLI's token is restricted to its URL, so a request with it says
    /// it comes from there.
    #[test]
    fn the_clis_token_goes_with_its_referer() {
        let config = config_dir("report-referer");
        let out = sent_nowhere(&config, &["--debug", "report", "A test"])
            .env("MAPBOX_CLI_TOKEN", "pk.cli")
            .output()
            .expect("run mapbox");
        let err = stderr(&out);
        assert!(
            err.contains("[debug] Referer: https://cli.mapbox.com/"),
            "{err}"
        );
    }

    /// Someone ran it, so a rest telemetry might take does not hold it back:
    /// it sends with the CLI's token.
    #[test]
    fn a_cooldown_does_not_hold_report_back() {
        let config = config_dir("report-not-resting");
        write_state(
            &config,
            &format!(
                r#"{{"token":"pk.saved","overridden":true,"cooldowns":{{"report":{{"failures":3,"until":{}}}}}}}"#,
                now() + 3600
            ),
        );
        let out = sent_nowhere(&config, &["--debug", "report", "A test"])
            .output()
            .expect("run mapbox");
        let err = stderr(&out);
        assert!(
            err.contains("[debug] Referer: https://cli.mapbox.com/"),
            "{err}"
        );
        assert!(!err.contains("recently"), "{err}");
    }

    /// Nor does the endpoint's backoff: with the kept token dead, it asks
    /// for a new one there and then, and sends with it.
    #[test]
    fn report_fetches_through_the_backoff() {
        let config = config_dir("report-through-backoff");
        write_state(
            &config,
            &format!(
                r#"{{"token":"pk.dead","rejected":true,"overridden":true,"failures":3,"retry_at":{}}}"#,
                now() + 3600
            ),
        );
        let out = sent_nowhere(&config, &["--debug", "report", "A test"])
            .env("MAPBOX_INTERNAL_CLI_TOKEN_URL", token_endpoint("pk.fresh"))
            .output()
            .expect("run mapbox");
        let err = stderr(&out);
        assert!(
            err.contains("[debug] Referer: https://cli.mapbox.com/"),
            "{err}"
        );
        let state = std::fs::read_to_string(config.join("cli-token.json")).expect("the state");
        assert!(state.contains(r#""token":"pk.fresh""#), "{state}");
    }

    /// The user's own token is not restricted to anything of ours, and goes
    /// without the CLI token's Referer when it stands in.
    #[test]
    fn the_users_token_goes_without_a_referer() {
        let config = config_dir("report-no-referer");
        let out = sent_nowhere(
            &config,
            &["--debug", "--token", "sk.mine", "report", "A test"],
        )
        .output()
        .expect("run mapbox");
        let err = stderr(&out);
        assert!(err.contains("Using your own token instead"), "{err}");
        assert!(err.contains("[debug] POST"), "{err}");
        assert!(!err.contains("Referer"), "{err}");
    }

    /// A dry run fetches nothing, so with no CLI token kept the plan goes out
    /// with the user's, and a typed `--token` is not reported as ignored.
    #[test]
    fn a_dry_run_with_no_kept_token_plans_with_the_users() {
        let config = config_dir("report-dry-user");
        let out = run(
            &config,
            &["--token", "sk.mine", "report", "--dry-run", "A test"],
        );
        assert!(out.status.success(), "{}", stderr(&out));
        let err = stderr(&out);
        assert!(!err.contains("was not used"), "{err}");
        assert!(!err.contains("no access token was resolved"), "{err}");
    }
}
