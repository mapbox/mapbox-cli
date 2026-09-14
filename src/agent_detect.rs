//! Detect the AI coding agent (if any) driving this CLI invocation.
//!
//! Ported from `tilesets-cli`'s `agent_detect.py` (mapbox/tilesets-cli#219)
//! and `mapbox-sdk-js`'s `agent-detect.js` — keep all three in sync.

/// `(agent_id, [env_var, ...])`, in precedence order. Only presence is
/// checked, never a value — so a stray value like a newline can never reach
/// the `User-Agent` header.
///
/// `TERM_PROGRAM` (which would detect Warp) is deliberately absent: it's set
/// by most terminal emulators, not just Warp.
///
/// The last entry, `custom-agent`, is a catch-all for `AI_AGENT`/`AGENT` —
/// for the same reason, only their presence is checked, never read.
const ALLOWLIST: &[(&str, &[&str])] = &[
    ("antigravity", &["ANTIGRAVITY_AGENT"]),
    ("augment-cli", &["AUGMENT_AGENT"]),
    ("cline", &["CLINE_ACTIVE"]),
    ("cowork", &["CLAUDE_CODE_IS_COWORK"]),
    ("claude-code", &["CLAUDECODE", "CLAUDE_CODE"]),
    ("codex", &["CODEX_SANDBOX", "CODEX_CI", "CODEX_THREAD_ID"]),
    ("crush", &["CRUSH"]),
    ("gemini-cli", &["GEMINI_CLI"]),
    (
        "github-copilot",
        &["COPILOT_MODEL", "COPILOT_ALLOW_ALL", "COPILOT_GITHUB_TOKEN"],
    ),
    ("goose", &["GOOSE_TERMINAL"]),
    ("hermes-agent", &["HERMES_SESSION_ID"]),
    ("junie", &["JUNIE_DATA", "JUNIE_SHIM_PATH"]),
    ("kilo-code", &["KILOCODE_FEATURE"]),
    ("kiro", &["AGENT_CONTEXT_OUT"]),
    ("openclaw", &["OPENCLAW_SHELL"]),
    ("opencode", &["OPENCODE_CLIENT"]),
    ("pi", &["PI_CODING_AGENT"]),
    ("replit", &["REPL_ID"]),
    ("roo-code", &["ROO_ACTIVE"]),
    ("trae", &["TRAE_AI_SHELL_ID"]),
    ("vtcode", &["VTCODE"]),
    ("zed", &["ZED_TERM"]),
    ("cursor-cli", &["CURSOR_AGENT"]),
    ("cursor", &["CURSOR_TRACE_ID"]),
    ("custom-agent", &["AI_AGENT", "AGENT"]),
];

/// Returns the detected agent id, or `None`.
pub fn detect_agent() -> Option<&'static str> {
    find_agent(|var| std::env::var_os(var).is_some())
}

/// The lookup itself, taking a presence check rather than reading the
/// environment directly — so tests can check the table without touching the
/// real process environment, which other tests in the suite also read and
/// write.
fn find_agent(present: impl Fn(&str) -> bool) -> Option<&'static str> {
    ALLOWLIST
        .iter()
        .find(|(_, env_vars)| env_vars.iter().any(|var| present(var)))
        .map(|(agent_id, _)| *agent_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(present_vars: &[&str]) -> Option<&'static str> {
        find_agent(|var| present_vars.contains(&var))
    }

    #[test]
    fn no_indicators_returns_none() {
        assert_eq!(find(&[]), None);
    }

    /// Every entry's own vars reach it — the table has no dead rows and no
    /// typo'd env var name goes unnoticed.
    #[test]
    fn every_allowlist_entry_is_reachable_by_its_own_vars() {
        for (agent_id, env_vars) in ALLOWLIST {
            for var in *env_vars {
                assert_eq!(find(&[var]), Some(*agent_id), "{var} -> {agent_id}");
            }
        }
    }

    #[test]
    fn harness_var_wins_over_ai_agent_fallback() {
        assert_eq!(find(&["CLAUDECODE", "AI_AGENT"]), Some("claude-code"));
    }

    #[test]
    fn warp_was_dropped_term_program_is_not_a_safe_existence_only_signal() {
        assert_eq!(find(&["TERM_PROGRAM"]), None);
    }

    #[test]
    fn table_order_precedence_among_harness_vars() {
        assert_eq!(
            find(&["CURSOR_AGENT", "ANTIGRAVITY_AGENT"]),
            Some("antigravity")
        );
    }

    #[test]
    fn fallback_ai_agent_takes_precedence_over_agent() {
        assert_eq!(find(&["AI_AGENT", "AGENT"]), Some("custom-agent"));
    }

    /// The one test that touches the real process environment, to check the
    /// wiring between `detect_agent()` and `std::env` — the table itself is
    /// checked above without it. Cleared and restored around the one
    /// variable this sets, since this process may itself be an agent (e.g.
    /// Claude Code sets `CLAUDECODE`).
    #[test]
    fn detect_agent_reads_the_real_environment() {
        let saved: Vec<(&str, Option<std::ffi::OsString>)> = ALLOWLIST
            .iter()
            .flat_map(|(_, vars)| vars.iter().copied())
            .map(|var| (var, std::env::var_os(var)))
            .collect();
        for (var, _) in &saved {
            std::env::remove_var(var);
        }

        std::env::set_var("VTCODE", "");
        let result = detect_agent();

        for (var, value) in saved {
            match value {
                Some(value) => std::env::set_var(var, value),
                None => std::env::remove_var(var),
            }
        }

        assert_eq!(
            result,
            Some("vtcode"),
            "a blank value should still count as present"
        );
    }
}
