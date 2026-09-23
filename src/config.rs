//! `mapbox config` — settings that persist across shells and sessions.
//!
//! `MAPBOX_NO_UPDATE_CHECK=1` silences the update notice, but only for the
//! shell session that set it — there is no way to turn the check off once
//! and have it stay off. This is the persisted alternative: a small JSON
//! file beside the credentials, written through the same
//! [`crate::auth::write_private`] so it gets the same `0600` treatment.
//!
//! One setting today — `update-check` — with room for more: `get`/`set` take
//! a `key`, restricted by clap to [`KEYS`], so adding a second setting is a
//! new key and a new match arm rather than a new pair of subcommands.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgMatches, Command};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth;
use crate::output::{self, Mode};

pub const COMMAND: &str = "config";

const CONFIG_FILE: &str = "config.json";

const UPDATE_CHECK_KEY: &str = "update-check";
const KEYS: &[&str] = &[UPDATE_CHECK_KEY];

const ON: &str = "on";
const OFF: &str = "off";

/// What's persisted. `None` means "never set" for a setting whose CLI-visible
/// default is `on` — distinct from `Some(true)`, which is someone turning it
/// back on after having turned it off, but read identically by
/// [`update_check_setting`].
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    update_check: Option<bool>,
}

fn config_path() -> Option<PathBuf> {
    Some(auth::config_dir_path()?.join(CONFIG_FILE))
}

/// The persisted config, or the all-default one when there is nothing on
/// disk yet or what's there doesn't parse — the same forgiving read
/// [`crate::update_check`]'s cache uses, and for the same reason: a
/// malformed file here should cost nothing more than falling back to
/// defaults, never a failing command.
fn read_config() -> Config {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Best-effort in the read, deliberate in the write: `config set` is the one
/// command whose entire job is writing this file, so unlike the cache, a
/// failure here is reported rather than swallowed.
fn write_config(config: &Config) -> Result<()> {
    let dir = auth::config_dir()?;
    let text = serde_json::to_string(config).context("could not serialize the config")?;
    auth::write_private(&dir.join(CONFIG_FILE), &text)
}

/// Pure half of [`update_check_enabled`], so the default can be pinned
/// without going through the filesystem.
fn update_check_setting(config: &Config) -> bool {
    config.update_check.unwrap_or(true)
}

/// Whether the update check may run at all, per the persisted setting.
/// [`crate::update_check`] checks this alongside `MAPBOX_NO_UPDATE_CHECK` —
/// either one saying no is enough to stop it.
pub fn update_check_enabled() -> bool {
    update_check_setting(&read_config())
}

fn on_off(enabled: bool) -> &'static str {
    if enabled {
        ON
    } else {
        OFF
    }
}

pub fn command() -> Command {
    let key_arg = || {
        Arg::new("key")
            .required(true)
            .value_parser(PossibleValuesParser::new(KEYS))
    };

    Command::new(COMMAND)
        .about("Get or set a persisted mapbox setting")
        .long_about(
            "Get or set a mapbox setting that persists across shells and sessions, \
             written to a file beside the stored credentials rather than an \
             environment variable that only lasts for the session it was set in.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("get")
                .about("Print a setting's current value")
                .arg(key_arg()),
        )
        .subcommand(
            Command::new("set")
                .about("Persist a setting")
                .arg(key_arg())
                .arg(Arg::new("value").required(true).value_parser([ON, OFF])),
        )
}

pub fn get(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let key = matches.get_one::<String>("key").expect("required");
    let config = read_config();

    match key.as_str() {
        UPDATE_CHECK_KEY => {
            let enabled = update_check_setting(&config);
            output::emit(
                mode,
                on_off(enabled),
                json!({ "key": key, "value": enabled }),
            )
        }
        _ => unreachable!("clap's value_parser restricts `key` to {KEYS:?}"),
    }
}

pub fn set(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let key = matches.get_one::<String>("key").expect("required");
    let value = matches.get_one::<String>("value").expect("required");
    let enabled = value == ON;

    let mut config = read_config();
    match key.as_str() {
        UPDATE_CHECK_KEY => config.update_check = Some(enabled),
        _ => unreachable!("clap's value_parser restricts `key` to {KEYS:?}"),
    }
    write_config(&config)?;

    output::emit(
        mode,
        &format!("{key} set to {}.", on_off(enabled)),
        json!({ "key": key, "value": enabled }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_config_reads_update_check_as_on() {
        assert!(update_check_setting(&Config::default()));
    }

    #[test]
    fn the_config_round_trips_and_tolerates_an_empty_one() {
        let off = Config {
            update_check: Some(false),
        };
        let text = serde_json::to_string(&off).expect("serialize");
        assert_eq!(text, r#"{"update_check":false}"#);
        let read: Config = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(read, off);

        // A file from before this key existed, or one with nothing set yet.
        let empty: Config = serde_json::from_str("{}").expect("an empty object");
        assert_eq!(empty.update_check, None);
        assert!(update_check_setting(&empty));
    }

    #[test]
    fn on_and_off_round_trip_through_on_off() {
        assert_eq!(on_off(true), ON);
        assert_eq!(on_off(false), OFF);
    }
}
