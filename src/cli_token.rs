//! The token for requests the CLI makes on its own behalf, as opposed to the
//! ones the user asked for.
//!
//! A user's `--token` and `MAPBOX_ACCESS_TOKEN` are for the commands they
//! run. A background request (the first is telemetry delivery) is not theirs,
//! so it never borrows them; it resolves its own token, highest first:
//!
//! 1. `MAPBOX_CLI_TOKEN` at run time. The override: a developer pointing at
//!    staging supplies a staging token themselves.
//! 2. The stored login for the active profile, when it is still good for
//!    another minute. It is never refreshed here: a refresh takes the
//!    credentials lock and spends a single-use refresh token, and a request
//!    nobody is waiting for must not be the reason the next command's login
//!    is gone.
//! 3. The token compiled in from `MAPBOX_CLI_TOKEN` at build time, so someone
//!    who never logged in still has one. A plain `cargo build` has none.
//!
//! # The bundled token is public
//!
//! Anything compiled into a distributed binary can be pulled out with
//! `strings`. The bundled `pk.` token must therefore be a dedicated one with
//! the minimum scopes, and nothing may ever treat it as a secret.

use crate::auth;

/// Seconds of remaining life below which a stored login is not used.
const EXPIRY_MARGIN_SECS: u64 = 60;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Source {
    Override,
    Login,
    Bundled,
}

/// The token a background request should carry, or `None` when there is
/// nothing to send it with.
// The first caller is telemetry delivery, which lands separately.
#[allow(dead_code)]
pub(crate) fn for_background(profile: Option<&str>) -> Option<String> {
    let override_token = std::env::var("MAPBOX_CLI_TOKEN").ok();
    // Read-only: a background request must not be what creates or
    // re-permissions the config directory on a machine that never logged in.
    let login = auth::load_credentials_readonly(profile).map(|c| c.access_token);
    choose(
        override_token.as_deref(),
        login.as_deref(),
        option_env!("MAPBOX_CLI_TOKEN"),
        now_secs(),
    )
    .map(|(_, token)| token)
}

/// The precedence itself, with the environment, the disk and the clock passed
/// in so each branch can be tested without any of them.
fn choose(
    override_token: Option<&str>,
    login: Option<&str>,
    bundled: Option<&str>,
    now_secs: u64,
) -> Option<(Source, String)> {
    fn usable(t: Option<&str>) -> Option<&str> {
        t.map(str::trim).filter(|t| !t.is_empty())
    }

    if let Some(token) = usable(override_token) {
        return Some((Source::Override, token.to_owned()));
    }
    if let Some(token) = usable(login) {
        // No `exp` claim means no expiry to honor, same as `needs_refresh`.
        let alive = auth::token_expires_at(token)
            .is_none_or(|exp| exp > now_secs.saturating_add(EXPIRY_MARGIN_SECS));
        if alive {
            return Some((Source::Login, token.to_owned()));
        }
    }
    usable(bundled).map(|token| (Source::Bundled, token.to_owned()))
}

#[allow(dead_code)]
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    const NOW: u64 = 1_000_000;

    fn token_expiring_at(exp: u64) -> String {
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        format!("pk.{payload}.signature")
    }

    fn token_without_exp() -> String {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"u":"someone"}"#);
        format!("pk.{payload}.signature")
    }

    fn won(
        override_token: Option<&str>,
        login: Option<&str>,
        bundled: Option<&str>,
    ) -> Option<(Source, String)> {
        choose(override_token, login, bundled, NOW)
    }

    #[test]
    fn override_wins_over_everything() {
        let login = token_expiring_at(NOW + 3600);
        assert_eq!(
            won(Some("pk.override"), Some(&login), Some("pk.bundled")),
            Some((Source::Override, "pk.override".into()))
        );
    }

    #[test]
    fn override_is_trimmed() {
        assert_eq!(
            won(Some("  pk.override\n"), None, None),
            Some((Source::Override, "pk.override".into()))
        );
    }

    #[test]
    fn empty_or_whitespace_override_is_ignored() {
        for blank in ["", "   ", "\n"] {
            assert_eq!(
                won(Some(blank), None, Some("pk.bundled")),
                Some((Source::Bundled, "pk.bundled".into()))
            );
        }
    }

    #[test]
    fn login_wins_over_bundled() {
        let login = token_expiring_at(NOW + 3600);
        assert_eq!(
            won(None, Some(&login), Some("pk.bundled")),
            Some((Source::Login, login))
        );
    }

    #[test]
    fn expired_login_falls_through_to_bundled() {
        let login = token_expiring_at(NOW - 1);
        assert_eq!(
            won(None, Some(&login), Some("pk.bundled")),
            Some((Source::Bundled, "pk.bundled".into()))
        );
    }

    #[test]
    fn login_expiring_within_the_margin_falls_through() {
        for exp in [NOW + 59, NOW + 60] {
            let login = token_expiring_at(exp);
            assert_eq!(
                won(None, Some(&login), Some("pk.bundled")),
                Some((Source::Bundled, "pk.bundled".into())),
                "exp = now + {}",
                exp - NOW
            );
        }
    }

    #[test]
    fn login_just_past_the_margin_is_used() {
        let login = token_expiring_at(NOW + 61);
        assert_eq!(
            won(None, Some(&login), Some("pk.bundled")),
            Some((Source::Login, login))
        );
    }

    #[test]
    fn login_without_exp_is_used() {
        let login = token_without_exp();
        assert_eq!(
            won(None, Some(&login), Some("pk.bundled")),
            Some((Source::Login, login))
        );
    }

    #[test]
    fn expired_login_with_no_bundled_token_is_none() {
        let login = token_expiring_at(NOW - 1);
        assert_eq!(won(None, Some(&login), None), None);
    }

    #[test]
    fn nothing_available_is_none() {
        assert_eq!(won(None, None, None), None);
        assert_eq!(won(Some(" "), Some(""), Some("")), None);
    }
}
