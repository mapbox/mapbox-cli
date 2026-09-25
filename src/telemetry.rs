//! What this CLI's `User-Agent` says about the environment, beyond its
//! version. `MAPBOX_CLI_NO_TELEMETRY` disables all of it.
//!
//! `crate::http` builds the client and attaches [`user_agent`]'s result.

use std::io::IsTerminal;

use crate::agent_detect;

/// The opt-out variable.
const MAPBOX_CLI_NO_TELEMETRY_ENV: &str = "MAPBOX_CLI_NO_TELEMETRY";

/// Values that mean "don't opt out" — same convention as `MAPBOX_YES=0` and
/// `install.ps1`'s `MAPBOX_NO_MODIFY_PATH=0`.
const NOT_AN_OPT_OUT: [&str; 6] = ["0", "f", "false", "n", "no", "off"];

/// Always sent, even when telemetry is off.
pub const PRODUCT_TOKEN: &str = concat!("mapbox-cli/", env!("CARGO_PKG_VERSION"));

/// Whether anything past [`PRODUCT_TOKEN`] may be sent.
pub(crate) fn telemetry_allowed() -> bool {
    match std::env::var_os(MAPBOX_CLI_NO_TELEMETRY_ENV) {
        None => true,
        Some(value) => {
            let value = value.to_string_lossy().trim().to_ascii_lowercase();
            value.is_empty() || NOT_AN_OPT_OUT.contains(&value.as_str())
        }
    }
}

/// `command_group` is `Some` only from [`crate::executor::dispatch`]; every
/// other caller passes `None` and gets no `command/` marker.
fn telemetry_markers(command_group: Option<&str>) -> Vec<String> {
    let mut markers = vec![os_marker(), arch_marker()];
    markers.extend(ci_marker());
    markers.extend(agent_detect::detect_agent().map(|agent| format!("agent/{agent}")));
    markers.push(terminal_marker());
    if let Some(group) = command_group {
        markers.push(format!("command/{group}"));
    }
    markers
}

fn os_marker() -> String {
    format!("os/{}", std::env::consts::OS)
}

/// Rust's `std::env::consts::ARCH` says `aarch64`; the docs and `uname -m`
/// on macOS say `arm64`.
fn arch_marker() -> String {
    format!("arch/{}", spelled_arch(std::env::consts::ARCH))
}

/// The CPU architecture as the event's `env.arch` spells it.
pub(crate) fn arch() -> &'static str {
    spelled_arch(std::env::consts::ARCH)
}

/// Whether this run is in CI, by the same rule as the `env/ci` marker.
pub(crate) fn in_ci() -> bool {
    ci_marker().is_some()
}

/// For `crate::telemetry_event`, which may not reach for stdout itself.
pub(crate) fn stdout_is_terminal() -> bool {
    std::io::stdout().is_terminal()
}

fn spelled_arch(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        other => other,
    }
}

/// Present when `CI` is set to anything, including empty — the convention
/// every major CI provider uses.
fn ci_marker() -> Option<String> {
    std::env::var_os("CI")
        .is_some()
        .then(|| "env/ci".to_string())
}

/// stdin and stdout are tracked separately because they can differ
/// (`cmd | less` vs `cmd < file`), as one marker so the pair stays together.
fn terminal_marker() -> String {
    format!(
        "stdin_tty/{}; stdout_tty/{}",
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal()
    )
}

/// The full `User-Agent`: [`PRODUCT_TOKEN`], then markers if allowed.
pub fn user_agent(command_group: Option<&str>) -> String {
    if telemetry_allowed() {
        assemble(&telemetry_markers(command_group))
    } else {
        assemble(&[])
    }
}

fn assemble(markers: &[String]) -> String {
    let mut agent = String::from(PRODUCT_TOKEN);
    for marker in markers {
        agent.push(' ');
        agent.push_str(marker);
    }
    agent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_agent_is_a_product_token_naming_this_crate_version() {
        let version = PRODUCT_TOKEN
            .strip_prefix("mapbox-cli/")
            .expect("the `mapbox-cli/` prefix log queries match on");
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
    }

    /// Restores the variable afterward — tests run in parallel and share
    /// the environment.
    #[test]
    fn the_switch_drops_the_markers_and_the_version_survives_it() {
        let opt_outs = ["1", "true", "yes", "on", " 1 ", "MAYBE"];
        let left_alone = ["0", "false", "F", "n", "no", "off", "", " "];

        let previous = std::env::var_os(MAPBOX_CLI_NO_TELEMETRY_ENV);
        let read = |value: &&str| {
            std::env::set_var(MAPBOX_CLI_NO_TELEMETRY_ENV, value);
            user_agent(None)
        };

        std::env::remove_var(MAPBOX_CLI_NO_TELEMETRY_ENV);
        let unset = user_agent(None);
        let opted_out: Vec<String> = opt_outs.iter().map(read).collect();
        let allowed: Vec<String> = left_alone.iter().map(read).collect();

        match previous {
            Some(value) => std::env::set_var(MAPBOX_CLI_NO_TELEMETRY_ENV, value),
            None => std::env::remove_var(MAPBOX_CLI_NO_TELEMETRY_ENV),
        }

        assert!(unset.starts_with(PRODUCT_TOKEN), "{unset}");
        for (value, agent) in opt_outs.iter().zip(&opted_out) {
            assert_eq!(agent, PRODUCT_TOKEN, "MAPBOX_CLI_NO_TELEMETRY={value:?}");
        }
        for (value, agent) in left_alone.iter().zip(&allowed) {
            assert_eq!(agent, &unset, "MAPBOX_CLI_NO_TELEMETRY={value:?}");
        }
    }

    #[test]
    fn os_and_arch_lead_every_marker_list() {
        let markers = telemetry_markers(None);
        assert_eq!(
            markers.first(),
            Some(&format!("os/{}", std::env::consts::OS))
        );
        assert_eq!(markers.get(1), Some(&arch_marker()));
    }

    #[test]
    fn aarch64_is_spelled_arm64_everything_else_passes_through() {
        assert_eq!(spelled_arch("aarch64"), "arm64");
        for arch in ["x86_64", "x86", "riscv64", "s390x"] {
            assert_eq!(spelled_arch(arch), arch);
        }
    }

    /// Restores `CI` afterward — this test may itself be running in CI.
    #[test]
    fn ci_marker_reads_presence_not_value() {
        let previous = std::env::var_os("CI");

        std::env::remove_var("CI");
        assert_eq!(ci_marker(), None);

        for value in ["true", "1", ""] {
            std::env::set_var("CI", value);
            assert_eq!(ci_marker(), Some("env/ci".to_string()), "CI={value:?}");
        }

        match previous {
            Some(value) => std::env::set_var("CI", value),
            None => std::env::remove_var("CI"),
        }
    }

    #[test]
    fn stdin_and_stdout_tty_ride_as_one_marker() {
        let expected = format!(
            "stdin_tty/{}; stdout_tty/{}",
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal()
        );
        assert_eq!(terminal_marker(), expected);
    }

    #[test]
    fn command_group_only_appears_when_one_is_given() {
        assert!(!telemetry_markers(None)
            .iter()
            .any(|marker| marker.starts_with("command/")));

        let markers = telemetry_markers(Some("styles"));
        assert_eq!(markers.last(), Some(&"command/styles".to_string()));
    }

    #[test]
    fn markers_ride_behind_the_version_one_space_apart() {
        assert_eq!(assemble(&[]), PRODUCT_TOKEN);
        assert_eq!(
            assemble(&["tty/1".to_string(), "ci/1".to_string()]),
            format!("{PRODUCT_TOKEN} tty/1 ci/1")
        );
    }
}
