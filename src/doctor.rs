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
//!
//! One [`Report`], read by both renderers. Building the text and the JSON
//! from two independently assembled values is exactly how the update-check
//! line first shipped forgetting `telemetry_allowed` — the JSON carried the
//! fact, the text line's own condition just didn't ask it. A single struct
//! removes the seam that let the two drift.

use std::time::Duration;

use anyhow::Result;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde::Serialize;

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

/// The fallback when neither `--timeout` nor `MAPBOX_TIMEOUT` named one —
/// short on purpose: this is a diagnostic a person is waiting on at a
/// terminal, not a request whose payload a command line bounds, so the
/// default fails fast rather than waiting out a stalled connection for as
/// long as a real command would. An explicit budget always wins, the same
/// as it does everywhere else — see [`http::requested`].
const DEFAULT_VERIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// The proxy variables `reqwest` reads, upper- and lowercase both — curl's
/// convention, which is also `libcurl`-derived `getenv` logic's, and reqwest
/// follows it. Listing both spellings explicitly rather than comparing
/// case-insensitively means a reader can see exactly what is checked without
/// having to know that convention exists. Named here, once, so a variable
/// added or removed from that behavior is at least this file's problem to
/// notice, not only `tests/proxy.rs`'s.
///
/// This still only answers "is a proxy variable set", not "would this
/// request actually use one" — `NO_PROXY` can exempt a specific host, and a
/// scheme-specific variable only applies to that scheme. Both are real gaps
/// against the true question, kept because reqwest has no public API that
/// answers "was a proxy applied to this request" for a caller to read back;
/// closing them means asking upstream for one, not maintaining a longer list
/// here.
const PROXY_VARS: &[&str] = &[
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

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

#[derive(Serialize)]
struct Build {
    version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: Option<&'static str>,
}

impl Build {
    fn current() -> Self {
        Build {
            version: env!("CARGO_PKG_VERSION"),
            channel: option_env!("MAPBOX_CLI_BUILD_ENV"),
        }
    }

    fn line(&self) -> String {
        format!(
            "mapbox {} ({})",
            self.version,
            self.channel.unwrap_or("dev")
        )
    }
}

#[derive(Serialize)]
struct TokenReport {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
}

impl TokenReport {
    /// Reuses [`auth::resolve_source`] rather than re-deriving the
    /// precedence — that function is the one place the order is decided,
    /// and `auth::whoami`'s own docs explain why a second opinion here
    /// would be worse than none.
    fn resolve(matches: &ArgMatches, use_login: bool, profile: Option<&str>) -> Self {
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
            Some((source, token)) => TokenReport {
                available: true,
                source: Some(source.as_str()),
                account: auth::token_account(token),
                usage: auth::token_usage(token).map(str::to_string),
                expires_at: auth::token_expires_at(token),
            },
            None => TokenReport {
                available: false,
                source: None,
                account: None,
                usage: None,
                expires_at: None,
            },
        }
    }

    fn line(&self) -> String {
        if self.available {
            format!(
                "Token:         available, from {} ({})",
                self.source.unwrap_or("unknown"),
                self.usage.as_deref().unwrap_or("unrecognized prefix"),
            )
        } else {
            "Token:         none available — run `mapbox auth login` or set MAPBOX_ACCESS_TOKEN"
                .to_string()
        }
    }
}

#[derive(Serialize)]
struct ProxyReport {
    active: Vec<&'static str>,
}

impl ProxyReport {
    fn current() -> Self {
        let active = PROXY_VARS
            .iter()
            .copied()
            .filter(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
            .collect();
        ProxyReport { active }
    }

    fn line(&self) -> String {
        if self.active.is_empty() {
            "Proxy:         none set".to_string()
        } else {
            format!("Proxy:         {}", self.active.join(", "))
        }
    }
}

#[derive(Serialize)]
struct SwitchesReport {
    telemetry_allowed: bool,
    update_check_env_opt_out: bool,
    update_check_persisted: bool,
}

impl SwitchesReport {
    fn current() -> Self {
        let env_opted_out =
            std::env::var(update_check::NO_UPDATE_CHECK_ENV).is_ok_and(|v| !v.trim().is_empty());
        SwitchesReport {
            telemetry_allowed: telemetry::telemetry_allowed(),
            update_check_env_opt_out: env_opted_out,
            update_check_persisted: config::update_check_enabled(),
        }
    }

    /// Whether the update check would actually run right now, on every
    /// switch a person can adjust — not the full gate `update_check::enabled`
    /// applies, which also asks about the build channel and whether stderr
    /// is a terminal. Those two are facts about this run, reported in
    /// `build`/the shell rather than here. `MAPBOX_CLI_NO_TELEMETRY` silences
    /// the update check too (see `update_check.rs`'s module docs), which the
    /// first version of this line forgot: it read `update_check_persisted`
    /// and `update_check_env_opt_out` alone, so `MAPBOX_CLI_NO_TELEMETRY=1`
    /// printed "Update check: on" for a check that would not run.
    fn update_check_on(&self) -> bool {
        self.update_check_persisted && !self.update_check_env_opt_out && self.telemetry_allowed
    }

    fn update_check_line(&self) -> String {
        let reason = if self.update_check_env_opt_out {
            format!(" ({} is set)", update_check::NO_UPDATE_CHECK_ENV)
        } else if !self.telemetry_allowed {
            " (MAPBOX_CLI_NO_TELEMETRY silences this too)".to_string()
        } else if !self.update_check_persisted {
            " (mapbox config set update-check off)".to_string()
        } else {
            String::new()
        };
        format!(
            "Update check:  {}{reason}",
            if self.update_check_on() { "on" } else { "off" }
        )
    }

    fn telemetry_line(&self) -> String {
        format!(
            "Telemetry:     {}",
            if self.telemetry_allowed { "on" } else { "off" }
        )
    }
}

#[derive(Serialize)]
struct ConnectivityReport {
    reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl ConnectivityReport {
    /// Whether `host` answers at all, through the same client and proxy
    /// handling every other request in this crate uses — so this reports
    /// what a real command would actually experience, not a bare TCP probe
    /// that a proxy-unaware check would get wrong in either direction. Any
    /// HTTP response counts as reachable, including an error one: this is
    /// asking whether the network path works, not whether the endpoint
    /// likes an unauthenticated request to its root.
    ///
    /// `error` is left out of the JSON entirely outside `--debug` (via
    /// `skip_serializing_if`), not set to `null` — the same shape the rest
    /// of this crate uses `Option` for, and what the docs promise.
    fn check(debug: bool, host: &str, timeout: Duration) -> Self {
        let outcome = http::client().and_then(|client| {
            client
                .get(host)
                .timeout(timeout)
                .send()
                .map_err(anyhow::Error::from)
        });

        match outcome {
            Ok(response) => ConnectivityReport {
                reachable: true,
                status: Some(response.status().as_u16()),
                error: None,
            },
            Err(e) => ConnectivityReport {
                reachable: false,
                status: None,
                error: debug.then(|| e.to_string()),
            },
        }
    }

    fn line(&self, host: &str) -> String {
        match self.status {
            Some(status) => format!("Reachable:     yes ({host} answered {status})"),
            None => format!("Reachable:     no ({host})"),
        }
    }
}

#[derive(Serialize)]
struct Report {
    build: Build,
    token: TokenReport,
    proxy: ProxyReport,
    switches: SwitchesReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    connectivity: Option<ConnectivityReport>,
}

impl Report {
    fn text(&self, host: &str) -> String {
        let mut lines = vec![
            self.build.line(),
            self.token.line(),
            self.proxy.line(),
            self.switches.update_check_line(),
            self.switches.telemetry_line(),
        ];
        if let Some(connectivity) = &self.connectivity {
            lines.push(connectivity.line(host));
        }
        lines.join("\n")
    }
}

/// `matches` is the top-level parse, the same one `whoami` reads `--token`
/// and `--timeout` from — `verify` is not global, so it comes from
/// `doctor_matches`, the subcommand's own.
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
    let timeout = http::requested(matches).unwrap_or(DEFAULT_VERIFY_TIMEOUT);

    let report = Report {
        build: Build::current(),
        token: TokenReport::resolve(matches, use_login, profile),
        proxy: ProxyReport::current(),
        switches: SwitchesReport::current(),
        connectivity: verify.then(|| ConnectivityReport::check(debug, &host, timeout)),
    };

    let text = report.text(&host);
    let json = serde_json::to_value(&report)?;
    output::emit(mode, &text, json)
}
