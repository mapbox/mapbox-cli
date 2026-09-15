//! What the CLI does with the standard proxy environment variables.
//!
//! Nothing in `src/` mentions a proxy: `http::build` never calls
//! `.no_proxy()`, so `reqwest`'s own system-proxy detection applies and
//! `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` are honoured. That
//! is an important behaviour for anyone on a corporate network and it rests
//! entirely on a library default nobody here chose — a single `.no_proxy()`
//! added to fix something else would remove it, and no existing test would
//! notice.
//!
//! So these tests assert the behaviour rather than the absence of a call. A
//! grep for `no_proxy` would pass just as well if the client were rebuilt
//! somewhere else, or if a future `reqwest` changed its default.
//!
//! **Nothing leaves the machine**, and that is what decided which tests are
//! here. Proving a *positive* — the proxy was used — needs only a loopback
//! listener, which refuses the tunnel as soon as it has learned which host was
//! asked for. Proving a *negative* — that `HTTP_PROXY` alone does not carry an
//! https request, or that `NO_PROXY` exempts a host — means the request goes
//! to the real API instead, so those tests would put the internet in the suite
//! to assert behaviour that belongs to `reqwest` rather than to this crate.
//! They are in `docs/commands.md` as prose instead.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A read that returns what arrived rather than blocking for a full buffer.
fn read_some(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("a read timeout");
    let mut buffer = [0u8; 1024];
    match stream.read(&mut buffer) {
        Ok(n) => buffer[..n].to_vec(),
        Err(_) => vec![],
    }
}

/// Runs `mapbox styles list` with one proxy variable set, and returns what the
/// fake proxy on the other end saw.
///
/// The token is deliberately a fake: a request that reaches the proxy has
/// already proved the point, and one that somehow bypassed it would get a 401
/// rather than touching a real account.
fn what_the_proxy_saw(
    variable: &str,
    scheme: &str,
    handler: fn(&mut TcpStream) -> String,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("the bound address").port();

    listener
        .set_nonblocking(true)
        .expect("a non-blocking listener");

    // Spawned rather than run to completion: the connection this is waiting
    // for only arrives while the child is running.
    let child = mapbox()
        .env(variable, format!("{scheme}://127.0.0.1:{port}"))
        .spawn()
        .expect("the binary runs");

    let seen = accept_within(&listener, ACCEPT_TIMEOUT, handler);
    let output = child.wait_with_output().expect("the binary exits");

    // The request cannot succeed — the proxy refuses it — so a success here
    // would mean the proxy was bypassed entirely.
    assert!(
        !output.status.success(),
        "the request should have failed at the fake proxy: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    seen.unwrap_or_else(|| {
        panic!(
            "nothing reached the fake proxy within {ACCEPT_TIMEOUT:?}, so {variable} was not \
             used. The CLI said: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// How long a connection may take to arrive before the variable is judged
/// ignored.
///
/// Generous, because a cold spawn on a loaded runner is not fast — and
/// **bounded**, which is the point. This blocked on `accept()` with no
/// deadline once, so "never connected" was indistinguishable from "has not
/// connected yet": on Windows it wedged the job for its full six-hour limit
/// instead of failing in seconds.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);

/// Waits for one connection, up to `timeout`, and runs `handler` on it.
/// `None` means nothing arrived — a result, not a reason to keep waiting.
fn accept_within(
    listener: &TcpListener,
    timeout: Duration,
    handler: fn(&mut TcpStream) -> String,
) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("a blocking stream to talk on");
                return Some(handler(&mut stream));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return None,
        }
    }
}

/// The binary, with the environment these tests need.
///
/// `env_remove` rather than `env_clear`, matching `tests/update_check.rs`.
/// Clearing takes `SystemRoot` with it on Windows, and without that the
/// socket and TLS stacks cannot initialise — so the CLI failed before it
/// could reach any proxy, which is what left the earlier version of this
/// test waiting for a connection that was never going to come.
fn mapbox() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mapbox"));
    cmd.args(["styles", "list", "--username", "someone", "-o", "json"])
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("NO_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .env_remove("no_proxy")
        .env_remove("MAPBOX_OUTPUT")
        .env("MAPBOX_ACCESS_TOKEN", "pk.a-fake-token-for-a-proxy-test")
        .env("MAPBOX_NO_UPDATE_CHECK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// An HTTP proxy is asked to tunnel an https request with `CONNECT`.
fn http_proxy(stream: &mut TcpStream) -> String {
    let request = read_some(stream);
    let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
    String::from_utf8_lossy(&request)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// **The behaviour this file exists for.** An `HTTPS_PROXY` in the environment
/// is used, and the proxy is asked for the host the CLI was going to reach.
#[test]
fn an_https_proxy_in_the_environment_is_used() {
    let seen = what_the_proxy_saw("HTTPS_PROXY", "http", http_proxy);

    assert!(
        seen.starts_with("CONNECT api.mapbox.com:443"),
        "the proxy should have been asked to tunnel to the API, saw: {seen:?}"
    );
}

/// SOCKS is **not** supported, and this pins the fact that the failure says so.
///
/// `reqwest` is built without its `socks` feature, so a `socks5://` proxy is
/// rejected when the connection is made rather than ignored. The message
/// carries `unsupported scheme socks5` from `reqwest` itself, which is the
/// part a user needs — a bare "the network failed" on a machine where every
/// other tool works would be a long afternoon.
///
/// If this ever starts failing because SOCKS began working, that is a feature
/// rather than a break: delete the test and document the support.
#[test]
fn a_socks_proxy_is_refused_with_a_reason() {
    let output = mapbox()
        // Port 9 (discard) rather than a live one: the scheme is rejected
        // before anything is dialled, so nothing needs to be listening.
        .env("ALL_PROXY", "socks5://127.0.0.1:9")
        .output()
        .expect("the binary runs");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported scheme socks5"),
        "the failure should name the unsupported scheme, got: {stderr}"
    );
    assert!(
        stderr.contains("ALL_PROXY"),
        "the advice should name the variable that caused it, got: {stderr}"
    );
}
