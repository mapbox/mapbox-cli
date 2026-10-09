//! The token for Mapbox API requests the CLI makes for itself rather than for
//! the user. Today that is telemetry delivery; requests that need no token,
//! like the update check, do not use this.
//!
//! A user's `--token` and `MAPBOX_ACCESS_TOKEN` are scoped to the command they
//! were given for, so a background request never borrows them. The stored
//! login is the CLI's own credential for this user and is fair to use. The
//! token is resolved highest first:
//!
//! 1. The stored login for the active profile, when it is still good for
//!    another minute. It is never refreshed here: a refresh takes the
//!    credentials lock and spends a single-use refresh token, and a request
//!    nobody is waiting for must not be the reason the next command's login
//!    is gone.
//! 2. `MAPBOX_CLI_TOKEN` at run time, for someone who is not logged in, such
//!    as a developer pointing at staging with a staging token.
//! 3. The token compiled in from `MAPBOX_CLI_BUNDLED_TOKEN` at build time, so
//!    someone who never logged in still has one. A plain `cargo build` has
//!    none. It is a different variable from the run-time one so that a token
//!    exported in a dev shell is not baked into every local build.
//!
//! # The bundled token is public
//!
//! Anything compiled into a distributed binary can be pulled out with
//! `strings`. The bundled token must therefore be a dedicated `pk.` token with
//! the minimum scopes, and nothing may ever treat it as a secret. `build.rs`
//! refuses anything that is not `pk.`.

use crate::auth;

/// Seconds of remaining life below which a stored login is not used.
const EXPIRY_MARGIN_SECS: u64 = 60;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Source {
    Login,
    Override,
    Bundled,
}

/// The token a background request should carry, or `None` when there is
/// nothing to send it with.
// The first caller is telemetry delivery, which lands separately.
#[allow(dead_code)]
pub(crate) fn for_background(profile: Option<&str>) -> Option<String> {
    // Read-only: a background request must not be what creates or
    // re-permissions the config directory on a machine that never logged in.
    let login = auth::load_credentials_readonly(profile).map(|c| c.access_token);
    let override_token = std::env::var("MAPBOX_CLI_TOKEN").ok();
    choose(
        login.as_deref(),
        override_token.as_deref(),
        option_env!("MAPBOX_CLI_BUNDLED_TOKEN"),
        now_secs(),
    )
    .map(|(_, token)| token)
}

/// The precedence itself, with the environment, the disk and the clock passed
/// in so each branch can be tested without any of them.
fn choose(
    login: Option<&str>,
    override_token: Option<&str>,
    bundled: Option<&str>,
    now_secs: u64,
) -> Option<(Source, String)> {
    fn usable(t: Option<&str>) -> Option<&str> {
        t.map(str::trim).filter(|t| !t.is_empty())
    }

    if let Some(token) = usable(login) {
        // No `exp` claim means no expiry to honor, as in
        // `auth::token_needs_refresh`.
        let alive = auth::token_expires_at(token)
            .is_none_or(|exp| exp > now_secs.saturating_add(EXPIRY_MARGIN_SECS));
        if alive {
            return Some((Source::Login, token.to_owned()));
        }
    }
    if let Some(token) = usable(override_token) {
        return Some((Source::Override, token.to_owned()));
    }
    usable(bundled).map(|token| (Source::Bundled, token.to_owned()))
}

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
        login: Option<&str>,
        override_token: Option<&str>,
        bundled: Option<&str>,
    ) -> Option<(Source, String)> {
        choose(login, override_token, bundled, NOW)
    }

    #[test]
    fn login_wins_over_everything() {
        let login = token_expiring_at(NOW + 3600);
        assert_eq!(
            won(Some(&login), Some("pk.override"), Some("pk.bundled")),
            Some((Source::Login, login))
        );
    }

    #[test]
    fn override_wins_over_bundled() {
        assert_eq!(
            won(None, Some("pk.override"), Some("pk.bundled")),
            Some((Source::Override, "pk.override".into()))
        );
    }

    #[test]
    fn expired_login_falls_through_to_override() {
        let login = token_expiring_at(NOW - 1);
        assert_eq!(
            won(Some(&login), Some("pk.override"), Some("pk.bundled")),
            Some((Source::Override, "pk.override".into()))
        );
    }

    #[test]
    fn override_is_trimmed() {
        assert_eq!(
            won(None, Some("  pk.override\n"), None),
            Some((Source::Override, "pk.override".into()))
        );
    }

    #[test]
    fn login_is_trimmed_and_its_expiry_still_honored() {
        let login = token_expiring_at(NOW - 1);
        assert_eq!(
            won(Some(&format!(" {login}\n")), None, Some("pk.bundled")),
            Some((Source::Bundled, "pk.bundled".into()))
        );
        let login = token_expiring_at(NOW + 3600);
        assert_eq!(
            won(Some(&format!(" {login}\n")), None, None),
            Some((Source::Login, login))
        );
    }

    #[test]
    fn blank_login_or_override_is_ignored() {
        for blank in ["", "   ", "\n"] {
            assert_eq!(
                won(Some(blank), Some(blank), Some("pk.bundled")),
                Some((Source::Bundled, "pk.bundled".into()))
            );
        }
    }

    #[test]
    fn expired_login_falls_through_to_bundled() {
        let login = token_expiring_at(NOW - 1);
        assert_eq!(
            won(Some(&login), None, Some("pk.bundled")),
            Some((Source::Bundled, "pk.bundled".into()))
        );
    }

    #[test]
    fn login_expiring_within_the_margin_falls_through() {
        for exp in [NOW + 59, NOW + 60] {
            let login = token_expiring_at(exp);
            assert_eq!(
                won(Some(&login), None, Some("pk.bundled")),
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
            won(Some(&login), None, Some("pk.bundled")),
            Some((Source::Login, login))
        );
    }

    #[test]
    fn login_without_exp_is_used() {
        let login = token_without_exp();
        assert_eq!(
            won(Some(&login), None, Some("pk.bundled")),
            Some((Source::Login, login))
        );
    }

    #[test]
    fn expired_login_with_nothing_else_is_none() {
        let login = token_expiring_at(NOW - 1);
        assert_eq!(won(Some(&login), None, None), None);
    }

    #[test]
    fn nothing_available_is_none() {
        assert_eq!(won(None, None, None), None);
        assert_eq!(won(Some(""), Some(" "), Some("")), None);
    }
}
