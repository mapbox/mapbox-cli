//! `mapbox doctor` — a read-only snapshot of the environment the next
//! command would run in.
//!
//! `auth whoami` already answers which token the next command will use; this
//! answers the rest of what commonly goes wrong before a real command finds
//! out the hard way: a proxy variable that silently isn't doing what someone
//! thinks, or the update-check/telemetry switches resolving to something
//! other than what was intended.
//!
//! Nothing here is sent unless `--verify` asks for the one check that needs
//! a request — the same precedent `auth whoami --verify` already sets: a
//! diagnostic command should not itself be the request that reveals the
//! problem it exists to describe.

use std::time::Duration;

use anyhow::Result;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::json;

use crate::auth;
use crate::config;
use crate::http;
use crate::output::{self, Mode};
use crate::telemetry;
use crate::update_check;

pub const COMMAND: &str = "doctor";

/// The one host worth checking reachability against: what every bundled
/// spec resolves to when it names no `servers` entry of its own — see
/// `spec::parse_spec`'s own fallback. Not a `use` of that constant, because
/// this is diagnostic and that one is a build-time default; two names for
/// the same string would be one more thing to keep in sync for a value that
/// is already effectively pinned by nearly every operation's own tests.
const API_HOST: &str = "https://api.mapbox.com";

/// Overrides [`API_HOST`], for the same reason `update_check.rs`'s
/// `MAPBOX_INTERNAL_UPDATE_URL` exists: this crate makes no other outbound
/// request nothing here controls, so without a seam `--verify` could only
/// ever be tested against the real network — undocumented, and read only by
/// `tests/doctor.rs`.
const HOST_OVERRIDE_ENV: &str = "MAPBOX_INTERNAL_DOCTOR_URL";

fn api_host() -> String {
    std::env::var(HOST_OVERRIDE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| API_HOST.to_string())
}

/// A budget of its own, not [`http::client`]'s ordinary one: this is a
/// diagnostic a person is waiting on at a terminal, not a request whose
/// payload a command line bounds, so it fails fast rather than waiting out
/// a stalled connection for as long as a real command would.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// The proxy variables `http::build` actually reads — see that module's own
/// docs for why `.no_proxy()` is never called. Named here, once, so a
/// variable added there and forgotten here is at least this file's problem
/// to notice, not only `tests/proxy.rs`'s.
const PROXY_VARS: &[&str] = &["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "NO_PROXY"];

pub fn command() -> Command {
    Command::new(COMMAND)
        .about("Show the environment the next command would run in")
        .long_about(
            "A read-only snapshot of what the next command would see: which token wins \
             and its state, which proxy variables are in effect, and where the \
             update-check and telemetry switches currently stand. `--verify` additionally \
             checks that api.mapbox.com is reachable — the one part of this that makes a \
             request.",
        )
        .arg(
            Arg::new("verify")
                .long("verify")
                .action(ArgAction::SetTrue)
                .help("Also check that api.mapbox.com is reachable"),
        )
}

/// The token report, reusing [`auth::resolve_source`] rather than
/// re-deriving the precedence — that function is the one place the order is
/// decided, and `auth::whoami`'s own docs explain why a second opinion here
/// would be worse than none.
fn token_report(matches: &ArgMatches, use_login: bool, profile: Option<&str>) -> serde_json::Value {
    let flag = auth::typed_token(matches);
    let environment = flag
        .is_none()
        .then(|| matches.get_one::<String>("token").cloned())
        .flatten();
    let stored = auth::load_credentials(profile);

    match auth::resolve_source(
        flag.as_deref(),
        environment.as_deref(),
        stored.as_ref().map(|c| c.access_token.as_str()),
        use_login,
    ) {
        Some((source, token)) => json!({
            "available": true,
            "source": source.as_str(),
            "account": auth::token_account(token),
            "usage": auth::token_usage(token),
            "expires_at": auth::token_expires_at(token),
        }),
        None => json!({ "available": false }),
    }
}

fn proxy_report() -> serde_json::Value {
    let active: Vec<&str> = PROXY_VARS
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
        .collect();
    json!({ "active": active })
}

/// The two switches, and how each is currently set — not the full four-way
/// gate `update_check::enabled` applies, which also asks about the build
/// channel and whether stderr is a terminal. Those two are facts about this
/// run, not something a person adjusts, and belong in `build`/the shell
/// rather than here.
fn switches_report() -> serde_json::Value {
    let env_opted_out =
        std::env::var(update_check::NO_UPDATE_CHECK_ENV).is_ok_and(|v| !v.trim().is_empty());
    json!({
        "telemetry_allowed": telemetry::telemetry_allowed(),
        "update_check_env_opt_out": env_opted_out,
        "update_check_persisted": config::update_check_enabled(),
    })
}

fn build_report() -> serde_json::Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "channel": option_env!("MAPBOX_CLI_BUILD_ENV"),
    })
}

/// Whether `API_HOST` answers at all, through the same client and proxy
/// handling every other request in this crate uses — so this reports what a
/// real command would actually experience, not a bare TCP probe that a
/// proxy-unaware check would get wrong in either direction. Any HTTP
/// response counts as reachable, including an error one: this is asking
/// whether the network path works, not whether the endpoint likes an
/// unauthenticated request to its root.
fn connectivity_report(debug: bool, host: &str) -> serde_json::Value {
    let outcome = http::client().and_then(|client| {
        client
            .get(host)
            .timeout(VERIFY_TIMEOUT)
            .send()
            .map_err(anyhow::Error::from)
    });

    match outcome {
        Ok(response) => json!({ "reachable": true, "status": response.status().as_u16() }),
        Err(e) => json!({
            "reachable": false,
            "error": if debug { Some(e.to_string()) } else { None },
        }),
    }
}

/// `matches` is the top-level parse, the same one `whoami` reads `--token`
/// from — `verify` is not global, so it comes from `doctor_matches`, the
/// subcommand's own.
pub fn run(
    matches: &ArgMatches,
    use_login: bool,
    debug: bool,
    profile: Option<&str>,
    mode: Mode,
    doctor_matches: &ArgMatches,
) -> Result<()> {
    let verify = doctor_matches.get_flag("verify");
    let host = api_host();

    let token = token_report(matches, use_login, profile);
    let proxy = proxy_report();
    let switches = switches_report();
    let build = build_report();
    let connectivity = verify.then(|| connectivity_report(debug, &host));

    let mut lines = vec![format!(
        "mapbox {} ({})",
        build["version"].as_str().unwrap_or("unknown"),
        build["channel"].as_str().unwrap_or("dev")
    )];

    lines.push(if token["available"] == true {
        format!(
            "Token:         available, from {} ({})",
            token["source"].as_str().unwrap_or("unknown"),
            token["usage"].as_str().unwrap_or("unrecognized prefix"),
        )
    } else {
        "Token:         none available — run `mapbox auth login` or set MAPBOX_ACCESS_TOKEN"
            .to_string()
    });

    lines.push(match proxy["active"].as_array() {
        Some(active) if !active.is_empty() => format!(
            "Proxy:         {}",
            active
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => "Proxy:         none set".to_string(),
    });

    let update_check_on =
        switches["update_check_persisted"] == true && switches["update_check_env_opt_out"] == false;
    lines.push(format!(
        "Update check:  {}{}",
        if update_check_on { "on" } else { "off" },
        if switches["update_check_env_opt_out"] == true {
            format!(" ({} is set)", update_check::NO_UPDATE_CHECK_ENV)
        } else if switches["update_check_persisted"] == false {
            " (mapbox config set update-check off)".to_string()
        } else {
            String::new()
        }
    ));
    lines.push(format!(
        "Telemetry:     {}",
        if switches["telemetry_allowed"] == true {
            "on"
        } else {
            "off"
        }
    ));

    if let Some(connectivity) = &connectivity {
        lines.push(if connectivity["reachable"] == true {
            format!(
                "Reachable:     yes ({host} answered {})",
                connectivity["status"]
            )
        } else {
            format!("Reachable:     no ({host})")
        });
    }

    let mut json = json!({
        "build": build,
        "token": token,
        "proxy": proxy,
        "switches": switches,
    });
    if let Some(connectivity) = connectivity {
        json["connectivity"] = connectivity;
    }

    output::emit(mode, &lines.join("\n"), json)
}
