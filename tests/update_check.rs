//! End-to-end tests for the update notice.
//!
//! The unit tests in `src/update_check.rs` cover the decisions — which
//! version is newer, which of the four gates stopped a run, when the cache is
//! stale. What they cannot show is the part that makes the feature safe:
//! that the notice reaches **stderr and only stderr**, that the fetch happens
//! in a process the command does not wait for, and that a machine with no
//! route to the channel behaves exactly as it does today.
//!
//! Two things shape how these are written.
//!
//! **The channel is compiled in, and this is not a production build.** A
//! release binary asks `cli.mapbox.com`; `cargo test` builds one that asks
//! nothing at all. `MAPBOX_INTERNAL_UPDATE_URL` is the seam that lets a test
//! point the same code at a loopback server — see the module docs for why it
//! exists.
//!
//! **The notice needs a terminal**, by design, and a test process has pipes
//! on every stream. `script` provides a pseudo-terminal without taking a pty
//! crate on, the same way `tests/non_interactive.rs` reaches the confirmation
//! prompt; the command's own stdout is diverted to a file inside the session
//! so the two streams stay separable, which is what makes "never stdout"
//! checkable at all.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A version no release will ever carry, so a notice about it can only have
/// come from this test's own manifest or cache.
const NEWER: &str = "99.9.9";

/// The line the notice opens with. Not the whole message: the prose is
/// allowed to change without a test standing in the way.
const NOTICE_MARK: &str = "A newer mapbox is available";

fn scratch(name: &str) -> PathBuf {
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("update-check-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("config")).expect("create the scratch config dir");
    home
}

fn config_dir(home: &Path) -> PathBuf {
    home.join("config")
}

fn cache_file(home: &Path) -> PathBuf {
    config_dir(home).join("update-check.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_secs()
}

/// Writes the cache this binary would have written, so a test can start from
/// "the channel was asked, and it is ahead of us".
fn seed_cache(home: &Path, latest: &str, checked_at: u64, notified_at: u64) {
    let json =
        format!(r#"{{"checked_at":{checked_at},"notified_at":{notified_at},"latest":"{latest}"}}"#);
    std::fs::write(cache_file(home), json).expect("seed the cache");
}

fn read_cache(home: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(cache_file(home)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The real binary, with the developer's own environment cleared for the
/// reason `schema_contract.rs` clears it, and pointed at a scratch config
/// directory so nothing here can read or write a real credential store.
fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.env_remove("MAPBOX_ACCESS_TOKEN")
        .env_remove("MapboxAccessToken")
        .env_remove("MAPBOX_USERNAME")
        .env_remove("MAPBOX_OUTPUT")
        .env_remove("MAPBOX_NO_UPDATE_CHECK")
        .env_remove("MAPBOX_CLI_NO_TELEMETRY")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("MAPBOX_CONFIG_DIR", config_dir(home));
    cmd
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

/// Writes the persisted opt-out through the real `mapbox config set`, rather
/// than hand-writing `config.json` the way [`seed_cache`] hand-writes the
/// cache — proving the two commands agree on the file, not just that this
/// test's idea of its shape does.
fn set_update_check(home: &Path, value: &str) {
    let out = command(home)
        .args(["config", "set", "update-check", value])
        .output()
        .expect("run mapbox config set");
    assert!(
        out.status.success(),
        "mapbox config set update-check {value} failed: {}",
        stderr(&out)
    );
}

/// A loopback stand-in for `<channel>/latest/manifest.json`.
///
/// Serves one request and reports the request head it saw, so a test can
/// assert not only that the fetch happened but what it sent — the user agent
/// above all, since this is the one request the CLI makes that nobody asked
/// for and it must still identify itself.
fn manifest_server(body: String) -> (std::thread::JoinHandle<String>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");

    let server = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return String::new();
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

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        String::from_utf8_lossy(&head).into_owned()
    });

    (server, format!("http://{addr}/latest/manifest.json"))
}

/// The channel manifest shape, cut to what this
/// reads plus enough of the rest to prove it is not reading positionally.
fn manifest(version: &str) -> String {
    format!(
        r#"{{"version":"{version}","commit":"deadbee","released":"2026-01-01T00:00:00Z",
            "artifacts":{{"aarch64-apple-darwin":{{"file":"mapbox.tar.gz","sha256":"00"}}}}}}"#
    )
}

/// A port nothing is listening on, for the offline case. Bound and dropped,
/// so the number is real and refused rather than filtered.
fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let addr = listener.local_addr().expect("the bound address");
    drop(listener);
    format!("http://{addr}/latest/manifest.json")
}

/// Waits for the detached refresher to land its cache, or gives up.
///
/// A poll rather than a fixed sleep: the child is deliberately not waited on
/// — that is the whole design — so there is nothing to join, and the time it
/// takes is a loopback round trip plus a process start.
///
/// `cfg(unix)` because its only caller is: the full loop needs a terminal to
/// print the second half of, and the pty comes from `script`. Without the
/// attribute this is dead code on Windows, where `-D warnings` makes that an
/// error rather than a warning.
#[cfg(unix)]
fn wait_for_cache(home: &Path) -> Option<serde_json::Value> {
    for _ in 0..100 {
        if let Some(cache) = read_cache(home) {
            if cache.get("latest").is_some() {
                return Some(cache);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

// ---------------------------------------------------------------- the child

/// The refresher is a whole mode of this binary, and this is all of it: ask
/// the manifest, write the cache, say nothing, exit 0.
#[test]
fn the_refresher_writes_the_cache_and_prints_nothing() {
    let home = scratch("refresher");
    let (server, url) = manifest_server(manifest(NEWER));

    let out = command(&home)
        .env("MAPBOX_INTERNAL_UPDATE_REFRESH", "1")
        .env("MAPBOX_INTERNAL_UPDATE_URL", &url)
        .output()
        .expect("run mapbox");

    assert!(
        out.status.success(),
        "the refresher failed: {}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "", "the refresher wrote to stdout");
    assert_eq!(stderr(&out), "", "the refresher wrote to stderr");

    let head = server.join().expect("the server thread");
    assert!(
        head.starts_with("GET /latest/manifest.json"),
        "asked for the wrong thing: {head}"
    );
    // The one request nobody asked for still says who it is — `http::client`
    // is the only place a client is built, and this proves the refresher goes
    // through it rather than around it.
    assert!(
        head.contains("mapbox-cli/"),
        "the refresher sent no mapbox-cli user agent: {head}"
    );

    let cache = read_cache(&home).expect("the refresher wrote a cache");
    assert_eq!(cache["latest"], NEWER);
    assert!(
        cache["checked_at"].as_u64().is_some_and(|at| at > 0),
        "the cache carries no check time: {cache}"
    );
}

/// No route to the channel is the ordinary case for a laptop on a plane, and
/// it has to cost nothing: no cache, no output, no failure.
#[test]
fn an_unreachable_channel_leaves_no_trace() {
    let home = scratch("unreachable");

    let out = command(&home)
        .env("MAPBOX_INTERNAL_UPDATE_REFRESH", "1")
        .env("MAPBOX_INTERNAL_UPDATE_URL", closed_port_url())
        .output()
        .expect("run mapbox");

    assert!(
        out.status.success(),
        "a refused connection failed the child"
    );
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
    assert!(
        read_cache(&home).is_none(),
        "a failed fetch wrote a cache anyway"
    );
}

/// A channel that answers with something this cannot read — a 404 page, a
/// gateway error, a manifest whose shape changed — is the same as no answer.
#[test]
fn a_manifest_that_does_not_parse_is_ignored() {
    for body in [
        "not json at all",
        r#"{"artifacts":{}}"#,
        r#"{"version":""}"#,
    ] {
        let home = scratch("unparseable");
        let (server, url) = manifest_server(body.to_string());

        let out = command(&home)
            .env("MAPBOX_INTERNAL_UPDATE_REFRESH", "1")
            .env("MAPBOX_INTERNAL_UPDATE_URL", &url)
            .output()
            .expect("run mapbox");

        assert!(out.status.success(), "{body:?} failed the child");
        let _ = server.join();
        assert!(
            read_cache(&home).is_none(),
            "{body:?} was written to the cache as a version"
        );
    }
}

/// The opt-outs are read by the process that makes the request, not only by
/// the one that decides to spawn it.
#[test]
fn the_switches_stop_the_child_too() {
    for (name, value) in [
        ("MAPBOX_NO_UPDATE_CHECK", "1"),
        ("MAPBOX_CLI_NO_TELEMETRY", "1"),
    ] {
        let home = scratch("child-switch");
        // A port nothing answers: if the child ignored the switch, the test
        // would still pass here — so the assertion below is on the cache,
        // which a live fetch is the only way to write.
        let (server, url) = manifest_server(manifest(NEWER));

        let out = command(&home)
            .env("MAPBOX_INTERNAL_UPDATE_REFRESH", "1")
            .env("MAPBOX_INTERNAL_UPDATE_URL", &url)
            .env(name, value)
            .output()
            .expect("run mapbox");

        assert!(out.status.success());
        assert!(
            read_cache(&home).is_none(),
            "{name}={value} did not stop the child from fetching"
        );
        drop(server);
    }
}

/// The same proof as [`the_switches_stop_the_child_too`], for the persisted
/// setting rather than an environment variable — and written through the
/// CLI's own `config set` instead of a hand-seeded file, so this is really
/// two commands agreeing rather than one test's assumption about both.
#[test]
fn the_persisted_opt_out_stops_the_child_too() {
    let home = scratch("child-config");
    set_update_check(&home, "off");
    let (server, url) = manifest_server(manifest(NEWER));

    let out = command(&home)
        .env("MAPBOX_INTERNAL_UPDATE_REFRESH", "1")
        .env("MAPBOX_INTERNAL_UPDATE_URL", &url)
        .output()
        .expect("run mapbox");

    assert!(out.status.success());
    assert!(
        read_cache(&home).is_none(),
        "a persisted `update-check off` did not stop the child from fetching"
    );
    drop(server);
}

// --------------------------------------------------------------- no terminal

/// Piped, scripted or in CI, the whole thing is off: no notice, no request,
/// and no spawn. This is the case that keeps the check out of build logs, and
/// it is checked by the absence of the cache write the notice would have
/// made.
#[test]
fn nothing_happens_when_stderr_is_not_a_terminal() {
    let home = scratch("piped");
    seed_cache(&home, NEWER, now(), 0);

    let out = command(&home)
        .env("MAPBOX_INTERNAL_UPDATE_URL", closed_port_url())
        .arg("--version")
        .output()
        .expect("run mapbox");

    assert!(out.status.success());
    assert!(
        !stderr(&out).contains(NOTICE_MARK),
        "a piped run printed the notice: {}",
        stderr(&out)
    );
    assert!(
        !stdout(&out).contains(NOTICE_MARK),
        "the notice reached stdout: {}",
        stdout(&out)
    );
    assert_eq!(
        read_cache(&home).expect("the seeded cache")["notified_at"],
        0,
        "a piped run recorded a notice it never printed"
    );
}

/// `--version` is what a piped caller reads to find out what it is running,
/// and it has to stay exactly one line whatever the cache says.
#[test]
fn a_piped_version_is_the_version_and_nothing_else() {
    let home = scratch("version");
    seed_cache(&home, NEWER, now(), 0);

    let out = command(&home)
        .arg("--version")
        .output()
        .expect("run mapbox");
    let printed = stdout(&out);

    assert_eq!(
        printed.lines().count(),
        1,
        "extra lines on stdout: {printed}"
    );
    assert!(printed.starts_with("mapbox "), "{printed}");
}

// ------------------------------------------------------------ at a terminal

/// Runs one `mapbox` command under a pseudo-terminal, returning what the
/// session showed and what the command wrote to stdout, separately.
///
/// `script` is the way to a pty without a crate. The two implementations
/// disagree about argument order — BSD wants the command after the typescript
/// file, util-linux wants `-c` — so both are tried and the first that
/// actually ran wins, which is what `expect` identifies. stdin is
/// `/dev/null`, so nothing here can block on a read.
///
/// `env` is passed through `sh`'s own environment rather than to `script`,
/// because `script` on some platforms scrubs what it hands the child.
#[cfg(unix)]
fn under_a_pty(
    home: &Path,
    out_path: &Path,
    args: &str,
    env: &[(&str, &str)],
    expect: &str,
) -> Option<(String, String)> {
    let exe = env!("CARGO_BIN_EXE_mapbox");
    let exports: String = env
        .iter()
        .map(|(name, value)| format!("{name}='{value}' "))
        .collect();
    let inner = format!("{exports}{exe} {args} > {}", out_path.display());

    let forms: [Vec<String>; 2] = [
        vec![
            "-q".into(),
            "/dev/null".into(),
            "sh".into(),
            "-c".into(),
            inner.clone(),
        ],
        vec!["-q".into(), "-c".into(), inner.clone(), "/dev/null".into()],
    ];

    forms.iter().find_map(|argv| {
        let _ = std::fs::remove_file(out_path);
        let output = Command::new("script")
            .args(argv)
            .env_remove("MAPBOX_ACCESS_TOKEN")
            .env_remove("MapboxAccessToken")
            .env_remove("MAPBOX_USERNAME")
            .env_remove("MAPBOX_OUTPUT")
            .env_remove("MAPBOX_NO_UPDATE_CHECK")
            .env_remove("MAPBOX_CLI_NO_TELEMETRY")
            .env("HOME", home)
            .env("MAPBOX_CONFIG_DIR", config_dir(home))
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;

        // `script` relays the whole session on its own stdout; the command's
        // own stdout is in the file.
        let session = format!("{}{}", stdout(&output), stderr(&output));
        let written = std::fs::read_to_string(out_path).unwrap_or_default();
        (session.contains(expect) || written.contains(expect)).then_some((session, written))
    })
}

/// The first thing to verify: a stale binary says so on stderr, and the
/// command's own exit code and stdout are untouched.
#[cfg(unix)]
#[test]
fn a_stale_binary_says_so_at_a_terminal_and_only_on_stderr() {
    let home = scratch("pty-stale");
    let out_path = home.join("stdout");
    // Checked a moment ago, so this run has nothing to refresh and the only
    // thing it can do is talk.
    seed_cache(&home, NEWER, now(), 0);

    let (session, written) = under_a_pty(
        &home,
        &out_path,
        "--version",
        // Pointed at a refused port: if the run were to fetch after all, it
        // could not succeed, so anything printed came from the cache.
        &[("MAPBOX_INTERNAL_UPDATE_URL", &closed_port_url())],
        "mapbox ",
    )
    .expect("neither `script` form ran the command");

    assert!(
        session.contains(NOTICE_MARK) && session.contains(NEWER),
        "no notice at a terminal: {session}"
    );
    assert!(
        session.contains("MAPBOX_NO_UPDATE_CHECK"),
        "the notice does not say how to turn it off: {session}"
    );
    assert!(
        !written.contains(NOTICE_MARK),
        "the notice landed on stdout, which a caller redirects: {written}"
    );
    assert!(
        written.starts_with("mapbox "),
        "the command's own output changed: {written}"
    );

    // Having said it, the run records that it did — the second half of
    // "once a day".
    let cache = read_cache(&home).expect("the cache survived");
    assert!(
        cache["notified_at"].as_u64().is_some_and(|at| at > 0),
        "the notice was not recorded: {cache}"
    );
}

/// A binary that is current says nothing, however recently the channel was
/// asked. The seeded version is this binary's own.
#[cfg(unix)]
#[test]
fn a_current_binary_says_nothing() {
    let home = scratch("pty-current");
    let out_path = home.join("stdout");
    let current = env!("CARGO_PKG_VERSION");
    seed_cache(&home, current, now(), 0);

    let (session, _) = under_a_pty(
        &home,
        &out_path,
        "--version",
        &[("MAPBOX_INTERNAL_UPDATE_URL", &closed_port_url())],
        "mapbox ",
    )
    .expect("neither `script` form ran the command");

    assert!(
        !session.contains(NOTICE_MARK),
        "a current binary was told it is out of date: {session}"
    );
}

/// Either switch silences it at a terminal too — which is the only place it
/// would ever have spoken.
#[cfg(unix)]
#[test]
fn either_switch_silences_the_notice() {
    for (name, value) in [
        ("MAPBOX_NO_UPDATE_CHECK", "1"),
        ("MAPBOX_CLI_NO_TELEMETRY", "1"),
    ] {
        let home = scratch("pty-switch");
        let out_path = home.join("stdout");
        seed_cache(&home, NEWER, now(), 0);

        let (session, _) = under_a_pty(
            &home,
            &out_path,
            "--version",
            &[
                ("MAPBOX_INTERNAL_UPDATE_URL", &closed_port_url()),
                (name, value),
            ],
            "mapbox ",
        )
        .expect("neither `script` form ran the command");

        assert!(
            !session.contains(NOTICE_MARK),
            "{name}={value} did not silence the notice: {session}"
        );
    }
}

/// The persisted opt-out silences the notice at a terminal too, the same as
/// either environment switch does above.
#[cfg(unix)]
#[test]
fn the_persisted_opt_out_silences_the_notice() {
    let home = scratch("pty-config");
    let out_path = home.join("stdout");
    seed_cache(&home, NEWER, now(), 0);
    set_update_check(&home, "off");

    let (session, _) = under_a_pty(
        &home,
        &out_path,
        "--version",
        &[("MAPBOX_INTERNAL_UPDATE_URL", &closed_port_url())],
        "mapbox ",
    )
    .expect("neither `script` form ran the command");

    assert!(
        !session.contains(NOTICE_MARK),
        "a persisted `update-check off` did not silence the notice: {session}"
    );
}

/// The whole loop, as a person would meet it: a run with a cold cache
/// refreshes in the background and says nothing, and the run after it is the
/// one that tells them.
#[cfg(unix)]
#[test]
fn the_first_run_refreshes_and_the_next_one_reports() {
    let home = scratch("pty-loop");
    let out_path = home.join("stdout");
    let (server, url) = manifest_server(manifest(NEWER));

    let (first, _) = under_a_pty(
        &home,
        &out_path,
        "--version",
        &[("MAPBOX_INTERNAL_UPDATE_URL", &url)],
        "mapbox ",
    )
    .expect("neither `script` form ran the command");

    assert!(
        !first.contains(NOTICE_MARK),
        "the run that fetched also reported, so it waited on the network: {first}"
    );

    let _ = server.join();
    let cache = wait_for_cache(&home).expect("the detached refresher never wrote a cache");
    assert_eq!(cache["latest"], NEWER);

    let (second, _) = under_a_pty(
        &home,
        &out_path,
        "--version",
        &[("MAPBOX_INTERNAL_UPDATE_URL", &closed_port_url())],
        "mapbox ",
    )
    .expect("neither `script` form ran the command");

    assert!(
        second.contains(NOTICE_MARK) && second.contains(NEWER),
        "the run after the refresh said nothing: {second}"
    );
}

/// Twice in a row is once: the second run inside the day is silent, so a
/// stale binary is a note rather than a nag.
#[cfg(unix)]
#[test]
fn the_notice_is_printed_at_most_once_a_day() {
    let home = scratch("pty-once");
    let out_path = home.join("stdout");
    seed_cache(&home, NEWER, now(), 0);
    let url = closed_port_url();

    let (first, _) = under_a_pty(
        &home,
        &out_path,
        "--version",
        &[("MAPBOX_INTERNAL_UPDATE_URL", &url)],
        "mapbox ",
    )
    .expect("neither `script` form ran the command");
    assert!(first.contains(NOTICE_MARK), "no notice on the first run");

    let (second, _) = under_a_pty(
        &home,
        &out_path,
        "--version",
        &[("MAPBOX_INTERNAL_UPDATE_URL", &url)],
        "mapbox ",
    )
    .expect("neither `script` form ran the command");
    assert!(
        !second.contains(NOTICE_MARK),
        "the notice repeated within the day: {second}"
    );
}
