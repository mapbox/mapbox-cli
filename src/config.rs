//! `mapbox config` — settings that persist across shells and sessions.
//!
//! `MAPBOX_NO_UPDATE_CHECK=1` silences the update notice, but only for the
//! shell session that set it — there is no way to turn the check off once
//! and have it stay off. This is the persisted alternative: a small JSON
//! file beside the credentials, written through the same
//! [`crate::auth::write_private`] so it gets the same `0600` treatment.
//!
//! Two settings — `update-check` and `telemetry` — with room for more:
//! `get`/`set`/`unset` take a `key`, restricted by clap to [`KEYS`], so a new
//! setting is a new key and a new match arm rather than a new subcommand.
//! `list` needs no key at all: it walks [`KEYS`] and reports every setting's
//! current value in one call, which `get` cannot — the whole reason it
//! exists alongside `get`/`set` rather than waiting for a second setting to
//! make the gap visible.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgMatches, Command};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth;
use crate::output::{self, Mode};

pub const COMMAND: &str = "config";

const CONFIG_FILE: &str = "config.json";

const UPDATE_CHECK_KEY: &str = "update-check";
const TELEMETRY_KEY: &str = "telemetry";
const KEYS: &[&str] = &[UPDATE_CHECK_KEY, TELEMETRY_KEY];

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    telemetry: Option<bool>,
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

/// Whether the run's telemetry event may be recorded, per the persisted
/// setting. [`crate::telemetry_event`] checks it alongside `MAPBOX_CLI_NO_TELEMETRY`.
pub fn telemetry_enabled() -> bool {
    read_config().telemetry.unwrap_or(true)
}

fn on_off(enabled: bool) -> &'static str {
    if enabled {
        ON
    } else {
        OFF
    }
}

/// `key`'s current value in `config`, resolved to its default the same way
/// every getter here does. Shared by [`get`] and [`list`] so the two cannot
/// answer a key differently.
fn resolve(config: &Config, key: &str) -> bool {
    match key {
        UPDATE_CHECK_KEY => update_check_setting(config),
        TELEMETRY_KEY => config.telemetry.unwrap_or(true),
        _ => unreachable!("clap's value_parser restricts `key` to {KEYS:?}"),
    }
}

/// Clears `key` back to "never set" in `config`, in place. The counterpart
/// to `set`'s `Some(enabled)` — distinct from setting a key to its default
/// value, which [`resolve`] would read identically but which
/// `serde(skip_serializing_if)` would not write identically: a later default
/// change reaches only a key that was actually cleared.
fn clear(config: &mut Config, key: &str) {
    match key {
        UPDATE_CHECK_KEY => config.update_check = None,
        TELEMETRY_KEY => config.telemetry = None,
        _ => unreachable!("clap's value_parser restricts `key` to {KEYS:?}"),
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
        .subcommand(Command::new("list").about("List every setting and its current value"))
        .subcommand(
            Command::new("unset")
                .about("Clear a setting back to its default")
                .long_about(
                    "Clear a setting back to its default, rather than setting it to that \
                     default value explicitly — the difference matters the next time this \
                     CLI changes what the default is: a cleared key picks up the new default, \
                     a key explicitly set to the old default value does not.",
                )
                .arg(key_arg()),
        )
}

pub fn get(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let key = matches.get_one::<String>("key").expect("required");
    let config = read_config();
    let enabled = resolve(&config, key);

    output::emit(
        mode,
        on_off(enabled),
        json!({ "key": key, "value": enabled }),
    )
}

pub fn set(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let key = matches.get_one::<String>("key").expect("required");
    let value = matches.get_one::<String>("value").expect("required");
    let enabled = value == ON;

    let mut config = read_config();
    match key.as_str() {
        UPDATE_CHECK_KEY => config.update_check = Some(enabled),
        TELEMETRY_KEY => config.telemetry = Some(enabled),
        _ => unreachable!("clap's value_parser restricts `key` to {KEYS:?}"),
    }
    write_config(&config)?;

    output::emit(
        mode,
        &format!("{key} set to {}.", on_off(enabled)),
        json!({ "key": key, "value": enabled }),
    )
}

pub fn list(mode: Mode) -> Result<()> {
    let config = read_config();
    let entries: Vec<Value> = KEYS
        .iter()
        .map(|key| json!({ "key": key, "value": resolve(&config, key) }))
        .collect();

    let text = KEYS
        .iter()
        .map(|key| format!("{key}\t{}", on_off(resolve(&config, key))))
        .collect::<Vec<_>>()
        .join("\n");

    output::emit(mode, &text, Value::Array(entries))
}

pub fn unset(matches: &ArgMatches, mode: Mode) -> Result<()> {
    let key = matches.get_one::<String>("key").expect("required");

    let mut config = read_config();
    clear(&mut config, key);
    write_config(&config)?;

    let enabled = resolve(&config, key);
    output::emit(
        mode,
        &format!("{key} cleared, now {} (default).", on_off(enabled)),
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
            ..Config::default()
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

    #[test]
    fn resolve_matches_update_check_setting_at_every_state() {
        for update_check in [None, Some(true), Some(false)] {
            let config = Config {
                update_check,
                ..Config::default()
            };
            assert_eq!(
                resolve(&config, UPDATE_CHECK_KEY),
                update_check_setting(&config)
            );
        }
    }

    /// `clear` and `set` leave different bytes on disk even when they leave
    /// the same *value*: this is the difference `unset` exists to offer, so
    /// it is worth pinning rather than only exercising through `resolve`,
    /// which cannot tell the two states apart by design.
    #[test]
    fn clear_removes_the_key_rather_than_writing_the_default() {
        let mut explicit_default = Config {
            update_check: Some(true),
            ..Config::default()
        };
        clear(&mut explicit_default, UPDATE_CHECK_KEY);
        assert_eq!(explicit_default, Config::default());
        assert_eq!(explicit_default.update_check, None);

        let mut explicit_off = Config {
            update_check: Some(false),
            ..Config::default()
        };
        clear(&mut explicit_off, UPDATE_CHECK_KEY);
        assert_eq!(explicit_off.update_check, None);
    }
}
