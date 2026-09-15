//! Feature flags for commands implemented ahead of a public-rollout decision.
//!
//! The gate is narrower than it reads. It withholds a command from the
//! binaries Mapbox publishes to its staging and production channels, and from
//! nothing else: `MAPBOX_CLI_BUILD_ENV` is what tells those two apart, read at
//! compile time via `option_env!`, and only Mapbox's own release pipeline sets
//! it. Anything else — `cargo build` here, a fork, a distribution packaging
//! this from source — leaves it unset, so `is_dev_build_for` answers true
//! and every flag is on whatever its `switch` says.
//!
//! So a `false` switch is a statement about what an official release ships,
//! not about what the code does. Flipping it to `true` and cutting a release
//! is the only thing that puts a flagged command in front of someone who
//! installed one.
//!
//! A [`Flag`] holds the switch and the OAuth scopes `auth login` should
//! request while it's on, so a new flag is one declaration in [`flags`].
//! `build_app` uses `.is_enabled()` to decide whether to register a flag's
//! subcommand at all — disabled means it's absent from the `clap` tree, not
//! just hidden from `--help`. `auth::requested_scopes` folds every flag's
//! `.oauth_scopes` into what `login` asks for.

/// One flag-gated feature: the switch, and the OAuth scopes `auth login`
/// should request only while it's active.
pub struct Flag {
    /// Read through [`is_enabled`](Flag::is_enabled), which folds in the
    /// dev-build override.
    switch: bool,
    /// Scopes `auth login` requests only while this flag is enabled.
    pub oauth_scopes: &'static [&'static str],
}

impl Flag {
    pub fn is_enabled(&self) -> bool {
        enabled_for(self.switch, option_env!("MAPBOX_CLI_BUILD_ENV"))
    }
}

/// True unless `build_env` names the release pipeline building for staging
/// or production.
fn is_dev_build_for(build_env: Option<&str>) -> bool {
    !matches!(build_env, Some("staging") | Some("production"))
}

fn enabled_for(switch: bool, build_env: Option<&str>) -> bool {
    switch || is_dev_build_for(build_env)
}

/// Every flag-gated feature. Flip a switch to `true`, then cut a release,
/// once that feature is ready for staging and production.
///
/// Nothing is gated today: `ACCOUNT_USAGE` is the only entry here and its
/// switch is on. The mechanism stays for the next command that needs a
/// staged rollout.
pub mod flags {
    use super::Flag;

    /// Gates `account_usage::COMMAND` (`mapbox usage`), which calls the
    /// Statistics API. `statistics:read` is what that call needs;
    /// `mapbox auth login`'s default scopes don't request it.
    pub const ACCOUNT_USAGE: Flag = Flag {
        switch: true,
        oauth_scopes: &["statistics:read"],
    };

    /// Every flag, so a caller (`auth::requested_scopes`) can fold them all
    /// in without naming each by hand. A flag left out of this list
    /// silently contributes no scopes.
    pub const ALL: &[&Flag] = &[&ACCOUNT_USAGE];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_and_an_unset_build_env_ignore_a_false_switch() {
        assert!(enabled_for(false, None));
        assert!(enabled_for(false, Some("dev")));
    }

    #[test]
    fn staging_and_production_hide_a_false_switch() {
        assert!(!enabled_for(false, Some("staging")));
        assert!(!enabled_for(false, Some("production")));
    }

    #[test]
    fn a_true_switch_is_enabled_everywhere() {
        for build_env in [None, Some("dev"), Some("staging"), Some("production")] {
            assert!(enabled_for(true, build_env));
        }
    }

    #[test]
    fn every_flag_with_oauth_scopes_is_reachable_through_all() {
        assert!(
            !flags::ACCOUNT_USAGE.oauth_scopes.is_empty(),
            "update this test if a scope-less flag is ever added on purpose"
        );
        let scopes_in_all: Vec<&str> = flags::ALL
            .iter()
            .flat_map(|f| f.oauth_scopes.iter().copied())
            .collect();
        for scope in flags::ACCOUNT_USAGE.oauth_scopes {
            assert!(
                scopes_in_all.contains(scope),
                "{scope} is not reachable through ALL, so it never reaches a login"
            );
        }
    }
}
