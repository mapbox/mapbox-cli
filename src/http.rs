//! The one place a client that talks to Mapbox gets built.
//!
//! Every request carries a `User-Agent` — see [`crate::telemetry`] for what
//! it says. `reqwest` sends none of its own, so a client built anywhere else
//! is a request that arrives anonymous; `no_module_builds_its_own_client`
//! guards against that.
//!
//! The other thing decided in one place is how long a request may take.
//! `reqwest`'s builder supplies a whole-request budget when nobody names one
//! — thirty seconds, in 0.12.28 — so before this module named its own, every
//! Mapbox request was running under a number nothing in this repo had chosen,
//! documented, or let a caller move. It is declared here now, split in two
//! because a connection that will not open and a body that is still arriving
//! are not the same failure, and `--timeout`/`MAPBOX_TIMEOUT` is how a caller
//! disagrees with either.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::ArgMatches;

use crate::telemetry;

/// The flag and the variable a caller moves the budget with.
pub const TIMEOUT_ARG: &str = "timeout";
pub const TIMEOUT_ENV: &str = "MAPBOX_TIMEOUT";

/// How long opening the connection may take, counted on its own.
///
/// Separate from the whole-request budget because the two failures are not
/// the same one and do not deserve the same patience. A connection that has
/// not opened is a route that goes nowhere, a proxy that is not listening or
/// a DNS answer that never came — none of which improves by waiting, and all
/// of which `reqwest` would otherwise leave bounded only by the whole-request
/// budget, since it declares no connect timeout of its own. Ten seconds is
/// well past a TLS handshake to a Mapbox edge over a bad mobile link, and
/// short enough that an unreachable host reads as unreachable rather than as
/// a slow API.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The whole-request budget — connect, send, and the response body — for a
/// request whose size the command line bounds.
///
/// A minute rather than the thirty seconds `reqwest` supplies when nobody
/// says: thirty was never a decision anyone here made, and six of the twelve
/// services answer with bytes rather than text. A style ZIP or a glyph range
/// over a hotel connection can take longer than half a minute and still be a
/// request worth finishing. A metadata `GET` that has not answered in a
/// minute is not slow, it is stuck.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The same budget for a request whose body is a file.
///
/// `sources upload-chunk`, `styles batch-upload-sprite` and `styles
/// upload-sprite-image` put whatever `--file` names on the wire, and nothing
/// about a command line bounds that. Fifteen minutes is roughly 110 MB on a
/// 1 Mbit/s uplink and 550 MB on 5 Mbit/s, which covers the sizes those
/// commands deal in. Past that the caller knows something no constant here
/// can, and `--timeout` is how they say it.
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(900);

/// The largest budget `--timeout` accepts, in seconds.
///
/// A day. There has to be a ceiling somewhere — `Duration::from_secs_f64`
/// panics on a value it cannot represent, and `1e400` is a thing someone can
/// type — and past a day the number is a typo rather than a budget.
const LONGEST_TIMEOUT_SECONDS: f64 = 86_400.0;

/// What a request is carrying, which is the whole of what separates the two
/// budgets.
///
/// An enum rather than the `bool` it stands in for: the call site reads
/// `Payload::File`, and a bare `true` two arguments into a call is the kind
/// of thing that gets transposed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Payload {
    /// No body, or one typed into `--data` — argv bounds it — and a response
    /// no larger than a tile, a glyph range or a style.
    Bounded,
    /// A file: named by `--file`, or read by `--data @<path>` / `--data @-`.
    /// Nothing bounds it. `executor::payload_of` decides which of the two
    /// `--data` cases a request is, since the body looks the same either way
    /// by the time it is built.
    File,
}

/// How long the request being built may take, in total.
///
/// `asked` is what the caller wanted, from [`requested`], and it wins
/// outright for either payload: someone who names a number means it for the
/// command they are running, and a CLI that quietly multiplied it for uploads
/// would be answering a question nobody asked. Which is why `MAPBOX_TIMEOUT`
/// set to two minutes shortens an upload as readily as it lengthens a
/// listing.
pub fn budget(asked: Option<Duration>, payload: Payload) -> Duration {
    asked.unwrap_or(match payload {
        Payload::Bounded => REQUEST_TIMEOUT,
        Payload::File => TRANSFER_TIMEOUT,
    })
}

/// The budget the caller asked for: `--timeout` if it was typed, else
/// `MAPBOX_TIMEOUT`, else nothing and the default stands.
///
/// The argument's id and the type clap parsed it into are named here and
/// nowhere else: `get_one` with the wrong type parameter is a panic at run
/// time rather than a compile error, and this is asked for from two places.
pub fn requested(matches: &ArgMatches) -> Option<Duration> {
    matches
        .get_one::<Duration>(TIMEOUT_ARG)
        .copied()
        .or_else(environment_timeout)
}

/// Seconds, as a caller writes them.
///
/// Public because clap parses `--timeout` with it: a typed value that makes
/// no sense should be a usage error naming the flag, on the one command it
/// was written on, and clap phrases that better than we would.
///
/// Fractional seconds are accepted the way `curl --max-time` accepts them —
/// a script polling a status endpoint has a reason to want a budget under a
/// second, and a whole number is the common case either way. Everything
/// `f64` will parse that is not a duration is refused here rather than
/// carried further: `nan` and `inf` both parse, and `Duration::from_secs_f64`
/// panics on either.
pub fn parse_timeout(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let seconds: f64 = text
        .parse()
        .map_err(|_| "expected a number of seconds, like `90` or `2.5`".to_string())?;

    if !seconds.is_finite() || seconds <= 0.0 || seconds > LONGEST_TIMEOUT_SECONDS {
        return Err(format!(
            "expected a number of seconds above 0 and no more than \
             {LONGEST_TIMEOUT_SECONDS:.0} (24 hours)"
        ));
    }

    Ok(Duration::from_secs_f64(seconds))
}

/// `MAPBOX_TIMEOUT`, read once for the life of the process.
///
/// Read once because it is asked for at two different moments — when the
/// client is built, and again when each request is built — and a caller who
/// mistyped it should be told so once rather than three times in front of one
/// command's output. An environment variable does not change under a running
/// CLI, so there is nothing to re-read.
fn environment_timeout() -> Option<Duration> {
    static RESOLVED: OnceLock<Option<Duration>> = OnceLock::new();
    *RESOLVED.get_or_init(|| read_timeout(std::env::var(TIMEOUT_ENV).ok().as_deref()))
}

/// The reading itself, given the variable rather than going to find it.
///
/// Deliberately not clap's `.env()` on the argument, for the reason
/// `MAPBOX_OUTPUT` is not: under `.env()` clap validates the variable with
/// the same parser as the flag, so `export MAPBOX_TIMEOUT=` — how a shell
/// clears one — and `MAPBOX_TIMEOUT=30s` would each be a usage error on
/// *every* command, including the ones needed to recover. A variable meant to
/// make scripts easier must never be able to brick the CLI.
///
/// `--yes`'s `FalseyValueParser` is no help here either, and it is worth
/// saying why the two switches take different routes. That parser works
/// because a boolean can read everything it does not recognise as one of its
/// two answers. A duration has no such reading: there is no number that
/// `sideways` obviously meant. So this warns and falls back, the way
/// `MAPBOX_OUTPUT` does, and what it falls back to is the default rather than
/// no timeout at all.
///
/// Taking the value as an argument keeps every one of those edges testable
/// without a test setting a process-wide variable that the tests running
/// beside it also read.
fn read_timeout(value: Option<&str>) -> Option<Duration> {
    let value = value?.trim();
    // A cleared variable, which is how a shell says "never mind".
    if value.is_empty() {
        return None;
    }

    match parse_timeout(value) {
        Ok(timeout) => Some(timeout),
        Err(why) => {
            eprintln!(
                "Warning: {TIMEOUT_ENV}={value} is not a timeout — {why}. \
                 Falling back to the default."
            );
            None
        }
    }
}

/// The budget a client carries for any request that does not name its own.
///
/// Split out from [`client`] so there is something a test can hold: `reqwest`
/// exposes no getter for what a built client's budget is, so the only way to
/// check that this one is ours rather than the builder's default is to check
/// the value before it is handed over.
///
/// [`TRANSFER_TIMEOUT`] is not reachable from here on purpose — a client does
/// not know what any one request will carry. [`budget`] decides that where
/// the request is built. What this covers is every request that never asks:
/// `auth`'s token refresh, `--verify`, client registration and code exchange,
/// none of which moves more than a few kilobytes.
fn client_timeout() -> Duration {
    environment_timeout().unwrap_or(REQUEST_TIMEOUT)
}

/// A blocking client with the `User-Agent` and the timeouts already set.
///
/// Returns rather than panics, unlike `reqwest::blocking::Client::new()`:
/// a failed build here is a platform problem (no TLS backend, no runtime),
/// not a transport one, so there's no URL to redact and no connectivity to
/// suggest checking.
pub fn client() -> Result<reqwest::blocking::Client> {
    client_for(None)
}

/// [`client`], naming the command group for [`crate::executor::dispatch`] —
/// the only caller with one to report.
pub fn client_for(command_group: Option<&str>) -> Result<reqwest::blocking::Client> {
    build(client_timeout(), command_group)
}

/// The client itself, with its budget named rather than resolved.
///
/// The one place a `reqwest` client is constructed, and the one place the two
/// timeouts are declared. Before they were, `ClientBuilder`'s own thirty
/// seconds applied to every Mapbox request this CLI made — a number nothing
/// here chose and nobody could move, which a `reqwest` upgrade could have
/// changed without a line of this repo appearing in the diff.
fn build(timeout: Duration, command_group: Option<&str>) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(telemetry::user_agent(command_group))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .context("Could not start an HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    /// The header on the wire, from the client every caller uses: the builder
    /// call being right is not the same claim as the request carrying it.
    ///
    /// Asserts the product token rather than the whole header: that is the
    /// part that is sent whatever the environment says, so it neither races
    /// the test above nor has to be revisited when a marker starts riding
    /// behind it.
    #[test]
    fn a_request_carries_the_user_agent() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");

        // On a thread, because the head cannot be read until the request is
        // in flight, and `send` does not return until it is answered.
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the client's connection");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));

            // A byte at a time, stopping at the blank line: reading in chunks
            // would block waiting for a body this request does not have.
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }

            let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
            String::from_utf8_lossy(&head).into_owned()
        });

        client()
            .expect("a client")
            .get(format!("http://{addr}/"))
            .send()
            .expect("the loopback answer");

        let head = server.join().expect("the server thread");
        assert!(
            head.to_ascii_lowercase()
                .contains(&format!("user-agent: {}", telemetry::PRODUCT_TOKEN)),
            "no mapbox-cli User-Agent in the request head:\n{head}"
        );
    }

    /// What `reqwest::blocking::ClientBuilder` applies when nobody names a
    /// budget, as of 0.12.28: `Timeout::default()` at `src/blocking/client.rs`
    /// line 1501, reached from `execute_request` at line 1438 through
    /// `req.timeout().copied().or(self.timeout.0)`.
    ///
    /// Written down here so that the number this module is *not* running on
    /// is a fact a test can point at rather than a claim in a comment.
    const REQWEST_OWN_DEFAULT: Duration = Duration::from_secs(30);

    /// A listener that accepts and then says nothing at all.
    ///
    /// The failure this is standing in for is a Mapbox edge that takes the
    /// connection and never answers, which is the case a connect timeout
    /// cannot catch and the whole-request budget exists for. Returned rather
    /// than dropped: dropping the listener closes the port, and the client
    /// would get a refused connection instead of a silence.
    fn a_server_that_never_answers() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        (listener, format!("http://{addr}/"))
    }

    /// The budget is this module's own, and it is not the one that would
    /// apply if nobody had said.
    ///
    /// The point is the second half. `reqwest` supplies thirty seconds to a
    /// builder that names none, so before this module named one, every Mapbox
    /// request ran under a number nothing here had chosen — and a `reqwest`
    /// upgrade that moved it would have moved this CLI's behaviour with no
    /// line of this repo in the diff. Now the number is ours, and this fails
    /// if it ever silently becomes theirs again.
    #[test]
    fn the_budget_is_declared_here_rather_than_inherited() {
        assert_eq!(
            client_timeout(),
            REQUEST_TIMEOUT,
            "the client took a budget from somewhere other than this module"
        );
        assert_ne!(
            REQUEST_TIMEOUT, REQWEST_OWN_DEFAULT,
            "the declared budget and reqwest's own are the same number, so \
             nothing here can tell whether the declaration is still being made"
        );

        // The order is the design: a connection has to open well inside the
        // budget for the request, and an upload has to be given more room
        // than a listing, or there was no reason to have three constants.
        assert!(CONNECT_TIMEOUT < REQUEST_TIMEOUT);
        assert!(REQUEST_TIMEOUT < TRANSFER_TIMEOUT);
    }

    /// The budget is enforced, not merely declared.
    ///
    /// A quarter of a second rather than the real minute: what is being
    /// checked is that the value handed to the builder is the one the request
    /// runs out of, and a test that waited the real budget to prove it would
    /// be the slowest thing in the suite by two orders of magnitude.
    #[test]
    fn a_request_that_runs_out_of_time_says_so() {
        let (_listener, url) = a_server_that_never_answers();

        let started = std::time::Instant::now();
        let failure = build(Duration::from_millis(250), None)
            .expect("a client")
            .get(&url)
            .send()
            .expect_err("a server that never answers cannot have answered");

        assert!(
            failure.is_timeout(),
            "the request failed for some reason other than the budget: {failure}"
        );
        // Generous by a factor of twenty: this is asserting that the budget
        // applied at all, not measuring how promptly it fired.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the budget was declared but something else ended the request"
        );
    }

    /// A request may carry a budget of its own, and it is the one that
    /// applies.
    ///
    /// This is the mechanism `executor` puts `TRANSFER_TIMEOUT` on an upload
    /// with, so what it really pins is `reqwest`'s precedence:
    /// `execute_request` reads `req.timeout().copied().or(self.timeout.0)`,
    /// which is the request's own first. If that ever reversed, every upload
    /// would quietly fall back to the client's minute and this is what would
    /// say so.
    #[test]
    fn a_request_may_be_given_a_budget_of_its_own() {
        let (_listener, url) = a_server_that_never_answers();

        let started = std::time::Instant::now();
        let failure = build(Duration::from_secs(300), None)
            .expect("a client")
            .get(&url)
            .timeout(Duration::from_millis(250))
            .send()
            .expect_err("a server that never answers cannot have answered");

        assert!(failure.is_timeout(), "{failure}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the client's five minutes won over the request's quarter-second"
        );
    }

    /// An upload gets more room than a listing, and a caller who names a
    /// number gets that number for either.
    #[test]
    fn a_file_gets_the_longer_budget_unless_the_caller_said_otherwise() {
        assert_eq!(budget(None, Payload::Bounded), REQUEST_TIMEOUT);
        assert_eq!(budget(None, Payload::File), TRANSFER_TIMEOUT);

        // Both directions of "the caller wins": the named budget is longer
        // than the default for a listing and shorter than the default for an
        // upload, and it is used unchanged for both. A CLI that scaled it up
        // for uploads would be answering a question nobody asked.
        let asked = Duration::from_secs(120);
        assert_eq!(budget(Some(asked), Payload::Bounded), asked);
        assert_eq!(budget(Some(asked), Payload::File), asked);
    }

    /// What a caller may write, and what happens to everything else.
    ///
    /// The refusals matter more than the acceptances: `--timeout` is parsed
    /// by this function, so a value it wrongly accepted would either panic
    /// (`Duration::from_secs_f64` does, on `nan` and on anything it cannot
    /// represent) or become a budget nobody meant.
    #[test]
    fn the_seconds_a_caller_may_write() {
        for (written, expected) in [
            ("90", Duration::from_secs(90)),
            ("2.5", Duration::from_millis(2500)),
            // A stray space, which is what an env file or a here-doc leaves.
            (" 30 ", Duration::from_secs(30)),
            ("0.25", Duration::from_millis(250)),
            ("86400", Duration::from_secs(86_400)),
        ] {
            assert_eq!(
                parse_timeout(written),
                Ok(expected),
                "`{written}` did not parse as the duration it spells"
            );
        }

        for (written, why) in [
            ("", "nothing to read"),
            (" ", "nothing to read"),
            // A duration as another tool spells it, rejected rather than
            // guessed at: `30m` and `30ms` differ by one character and by
            // four orders of magnitude.
            ("30s", "a unit we do not read"),
            ("1m", "a unit we do not read"),
            ("two", "not a number at all"),
            ("0", "no time in which to do anything"),
            ("0.0", "no time in which to do anything"),
            ("-1", "a negative budget"),
            ("-0.5", "a negative budget"),
            // Both of these parse as `f64`, and `Duration::from_secs_f64`
            // panics on the first and on anything it cannot represent.
            ("nan", "a number that is not one"),
            ("inf", "past every ceiling"),
            ("1e400", "past every ceiling"),
            ("86401", "past the day this accepts"),
        ] {
            assert!(
                parse_timeout(written).is_err(),
                "`{written}` was accepted as a timeout, and it is {why}"
            );
        }
    }

    /// `MAPBOX_TIMEOUT` moves the budget, and no value of it can stop a
    /// command from running.
    ///
    /// That second half is the whole reason this is read by hand instead of
    /// through clap's `.env()`: under `.env()` the cleared and the mistyped
    /// cases below are a usage error on every command, `mapbox auth login`
    /// among them, and the way out of that is to know which variable to
    /// unset. Here each falls back to the default, having said so.
    ///
    /// Reads the value as an argument rather than setting the variable,
    /// because the tests beside this one run in the same process and read the
    /// same environment.
    #[test]
    fn the_environment_can_move_the_budget_and_cannot_break_it() {
        assert_eq!(read_timeout(Some("120")), Some(Duration::from_secs(120)));
        assert_eq!(read_timeout(Some(" 120 ")), Some(Duration::from_secs(120)));

        for cleared_or_unusable in [None, Some(""), Some(" "), Some("30s"), Some("sideways")] {
            assert_eq!(
                read_timeout(cleared_or_unusable),
                None,
                "MAPBOX_TIMEOUT={cleared_or_unusable:?} did something other than \
                 fall back to the default"
            );
        }
    }

    /// This module's own doc comment, checked rather than trusted: one
    /// client, built here, so no request can go out anonymous.
    ///
    /// Reads the directory rather than a list of modules, so a module added
    /// later is covered without anyone remembering to add it.
    #[test]
    fn no_module_builds_its_own_client() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut checked = 0;

        for entry in std::fs::read_dir(&src).expect("src/ is readable") {
            let path = entry.expect("a directory entry").path();
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            // This module is where the one client is allowed to be built.
            if path.file_name().is_some_and(|name| name == "http.rs") {
                continue;
            }

            let source = std::fs::read_to_string(&path).expect("a readable source file");
            for forbidden in ["Client::new()", "Client::builder()"] {
                assert!(
                    !source.contains(forbidden),
                    "{} builds its own client with `{forbidden}`; use \
                     `http::client()` so the request carries the User-Agent",
                    path.display()
                );
            }
            checked += 1;
        }

        // A guard that silently scanned nothing would pass forever.
        assert!(checked > 1, "expected to scan the crate's other modules");
    }
}
