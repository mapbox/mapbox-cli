use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::Rng;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{IsTerminal, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::output::{self, CliError, Mode};
use crate::remedy::{self, Remedy};

/// How long `login` waits for the browser to come back.
///
/// Unbounded before this: `accept()` blocks forever, so a login nobody
/// finished held the terminal until Ctrl-C — and in CI, where there is no
/// browser to finish it, held the job until the runner's own timeout killed
/// it. Five minutes is long enough for a password manager, an MFA prompt and
/// an account switch, and short enough to be a bug report rather than a
/// mystery.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// How long the callback request itself may take once something connects.
///
/// A separate, much shorter budget: the connection is local and the request is
/// one GET the browser has already composed. It bounds the damage rather than
/// preventing it — the first connection wins, so a health check or a port
/// scanner that connects and says nothing still ends the login, just in thirty
/// seconds instead of never. Serving more than one connection would fix that
/// properly and is not what this constant is for.
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(30);

const AUTHORIZATION_ENDPOINT: &str = "https://api.mapbox.com/oauth/2.1/authorize";
const TOKEN_ENDPOINT: &str = "https://api.mapbox.com/oauth/2.1/token";
const REGISTRATION_ENDPOINT: &str = "https://api.mapbox.com/oauth/register";
/// Where `--verify` sends the token. Reached with the token as a query
/// parameter, so every failure path out of it has to redact.
const VALIDATION_ENDPOINT: &str = "https://api.mapbox.com/tokens/v2";

// Every scope here must be one the Accounts API's dynamic client
// registration (DCR) will actually grant — a scope it doesn't recognize is
// silently dropped from the registered client rather than rejected.
// `tokens:write` is deliberately excluded — confirmed by direct POST /oauth/register
// against production that it's dropped from the granted scope regardless of being
// requested (DCR does not grant it at all; it only exists in the classic,
// role-gated token-creation path — see the `accounts create-token` etc. disablement
// in spec.rs).
// `scopes:list` is what `accounts list-scopes` actually requires — the scope
// audit behind `spec::UNSUPPORTED_OPERATIONS` recorded it as `tokens:read`,
// which is what the other two `accounts` operations need, and the command
// answered 403 for everyone as a result. It is registrable: a direct
// `POST /oauth/register` grants it back unchanged.
// Anyone logged in before this lands must run `mapbox auth login`
// again — the scope set is fixed when the client registers, so refreshing an
// existing token cannot widen it.
//
// `fonts:list` and `fonts:write` became registrable on 2026-09-08, which is
// what unblocks `fonts list-fonts`, `upload-font`, `delete-font`
// and `update-font-metadata` in `spec::UNSUPPORTED_OPERATIONS`.
// `update-font-metadata` briefly 404d in same-day testing right after the
// scope became registrable; a later retest with the same kind of token
// succeeded, so it ships alongside the other three rather than staying in
// `spec::WITHHELD_OPERATIONS`. Anyone logged in before this lands needs
// `mapbox auth login` again, for the same reason `scopes:list` above does.
const DEFAULT_SCOPES: &str = "styles:tiles styles:read styles:write styles:list fonts:read fonts:list fonts:write datasets:read datasets:write tokens:read scopes:list tilesets:read tilesets:write tilesets:list user-feedback:read";

/// [`DEFAULT_SCOPES`] plus every enabled flag's `oauth_scopes` — what
/// `mapbox auth login` actually requests.
///
/// `statistics:read` (`ACCOUNT_USAGE`'s scope, now on) has **not** been
/// confirmed against the Accounts API's registration allowlist, unlike
/// everything in `DEFAULT_SCOPES`. If DCR drops it silently (as it does for
/// `tokens:write` and others in `spec::UNSUPPORTED_OPERATIONS`), a login
/// done with the flag on would still leave `mapbox usage` unable to
/// authenticate. Confirm this against a real `mapbox auth login` before
/// this reaches a staging or production release.
fn requested_scopes() -> String {
    let gated: Vec<&'static [&'static str]> = crate::feature_flags::flags::ALL
        .iter()
        .filter(|flag| flag.is_enabled())
        .map(|flag| flag.oauth_scopes)
        .collect();
    scopes_with(&gated)
}

/// [`requested_scopes`], parameterised for testing — a build's own flags
/// can't be flipped from a test.
fn scopes_with(gated: &[&[&str]]) -> String {
    let mut scopes = DEFAULT_SCOPES.to_string();
    for scopes_list in gated {
        for scope in *scopes_list {
            scopes.push(' ');
            scopes.push_str(scope);
        }
    }
    scopes
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct ClientRegistration {
    client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Credentials {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

/// Restrict a directory to the owner (0700). No-op off Unix.
#[cfg(unix)]
fn harden_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("Failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn harden_dir(_path: &Path) -> Result<()> {
    Ok(())
}

/// Path for the scratch file `write_private` renames into place. Unique per
/// process and instant so concurrent writers never pick the same one.
fn temp_sibling(path: &Path) -> Result<PathBuf> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?
        .to_string_lossy();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(dir.join(format!(".{}.{}.{}.tmp", name, std::process::id(), nanos)))
}

/// Create a brand-new file that is owner-only from the instant it exists.
///
/// `create_new` means we never write through a pre-existing file or a symlink
/// planted in its place, and `mode` is applied by `open(2)` itself, so there is
/// no window where the file exists group- or world-readable.
fn create_private_file(path: &Path) -> Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .with_context(|| format!("Failed to create {}", path.display()))
}

/// Atomically replace `path` with `contents`, owner-readable only (0600 on
/// Unix; permissions are left to the OS elsewhere).
///
/// The write goes to a scratch file in the same directory and is then renamed
/// over the target, so a crash or a concurrent reader never observes a
/// half-written credentials file. Because rename swaps in a whole new inode,
/// the mode of the file being replaced is irrelevant — the result always
/// carries the mode set here.
pub(crate) fn write_private(path: &Path, contents: &str) -> Result<()> {
    let tmp = temp_sibling(path)?;

    let result = (|| -> Result<()> {
        let mut file = create_private_file(&tmp)?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("Failed to write to {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to flush {}", tmp.display()))?;
        drop(file);

        // `mode()` above is masked by the umask, so pin the mode explicitly.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("Failed to restrict permissions on {}", tmp.display()))?;
        }

        std::fs::rename(&tmp, path).with_context(|| {
            format!(
                "Failed to move {} into place at {}",
                tmp.display(),
                path.display()
            )
        })
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The environment's say over where credentials live. When it is set it *is*
/// the directory — not a parent to hang `.mapbox` off — so a container or a CI
/// job can point the whole credential store at a path it actually owns instead
/// of giving up on stored logins and exporting a raw token.
const CONFIG_DIR_ENV: &str = "MAPBOX_CONFIG_DIR";

/// `~/.mapbox`, or whatever [`CONFIG_DIR_ENV`] names, created and hardened.
///
/// A fixed directory under `$HOME` rather than the OS config directory: it is
/// one path to document instead of three, it is where comparable CLIs keep
/// their credentials (`~/.aws`, `~/.docker`), and being the same string
/// everywhere it spares every test that redirects `HOME` from having to know
/// which platform it is running on.
pub(crate) fn config_dir() -> Result<PathBuf> {
    let dir = config_dir_path().ok_or_else(|| anyhow!("Could not determine home directory"))?;
    prepare_config_dir(&dir)?;
    Ok(dir)
}

/// The same path, resolved and not created.
///
/// [`config_dir`] creates the directory and hardens it, which is right for
/// anything about to write a credential and wrong for anything merely looking
/// — `crate::update_check` reads a cache that usually is not there, on every
/// command, and a run that prints nothing must not leave a directory behind
/// as the trace of having thought about it.
pub(crate) fn config_dir_path() -> Option<PathBuf> {
    match std::env::var_os(CONFIG_DIR_ENV).filter(|value| !value.is_empty()) {
        Some(value) => Some(PathBuf::from(value)),
        None => Some(dirs::home_dir()?.join(".mapbox")),
    }
}

/// A plain file sitting where the credential directory belongs.
///
/// Its own type because the right amount to say depends on who is asking. An
/// `auth` command prints the whole [`Display`](std::fmt::Display) form: the
/// user is there to deal with credentials, so the fallback route through
/// `MAPBOX_CONFIG_DIR` is worth spelling out. Every other command prints
/// [`one_line`](Self::one_line) instead — a job authenticating through
/// `MAPBOX_ACCESS_TOKEN` works perfectly well despite the file, and repeating
/// seven lines of repair instructions on every invocation would bury the
/// output it came for.
#[derive(Debug)]
struct DirectoryBlocked {
    path: PathBuf,
}

impl DirectoryBlocked {
    /// The same fact in one line, fix included.
    ///
    /// The `mv` stays. Pointing at another command to *learn* the fix would
    /// send the reader to one that fails for this very reason, and "run
    /// `auth login`" reads as "you need to log in" when logging in is exactly
    /// what cannot help.
    fn one_line(&self) -> String {
        let shown = self.path.display();
        format!(
            "{shown} is a file, not a directory, so no stored credentials can be read. \
             Move it aside: mv {shown} {shown}.bak"
        )
    }
}

impl std::fmt::Display for DirectoryBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = self.path.display();
        write!(
            f,
            "{shown} is a file, but that is the directory credentials are stored in.\n\n\
             Move it aside to continue:\n\n    \
             mv {shown} {shown}.bak\n\n\
             Or set {CONFIG_DIR_ENV} to keep credentials somewhere else entirely."
        )
    }
}

impl std::error::Error for DirectoryBlocked {}

/// Creates the credential directory and restricts it to the owner.
///
/// Split from [`config_dir`] so the awkward case is testable without touching a
/// real `$HOME`: when something is already at the path and is *not* a
/// directory, `create_dir_all` answers `File exists`, which reads like a
/// reassurance rather than the problem it is. A plain `~/.mapbox` token file
/// left by older tooling is exactly that case, and it deserves naming — the fix
/// is one `mv` that nobody can guess from `os error 17`.
fn prepare_config_dir(dir: &Path) -> Result<()> {
    if dir.exists() && !dir.is_dir() {
        return Err(DirectoryBlocked {
            path: dir.to_path_buf(),
        }
        .into());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    // Best-effort: chmod is meaningless or forbidden on some mounts (NFS/SMB,
    // foreign-owned dirs), and that must not stop the CLI from working.
    if let Err(e) = harden_dir(dir) {
        eprintln!("Warning: {e}");
    }
    Ok(())
}

/// A profile name becomes part of a filename, so reject anything that could
/// escape the config dir or produce a nonsense path.
pub fn validate_profile(profile: Option<&str>) -> Result<()> {
    let Some(name) = profile else {
        return Ok(());
    };
    if name.is_empty() {
        return Err(anyhow!("Profile name cannot be empty"));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(anyhow!(
            "Invalid profile name {name:?}: must not contain '/', '\\' or '..'"
        ));
    }
    Ok(())
}

/// Map a profile to its credentials filename. The unnamed and `default`
/// profiles share the plain `credentials.json` name; every other profile gets
/// its own file alongside it. Pure — no filesystem access, so it is directly
/// testable.
fn credentials_filename(profile: Option<&str>) -> Result<String> {
    validate_profile(profile)?;
    Ok(match profile {
        None | Some("default") => "credentials.json".to_string(),
        Some(name) => format!("credentials-{name}.json"),
    })
}

fn credentials_path(profile: Option<&str>) -> Result<PathBuf> {
    Ok(config_dir()?.join(credentials_filename(profile)?))
}

/// One lock file per profile, so refreshing profile A never blocks profile B.
fn lock_filename(profile: Option<&str>) -> Result<String> {
    Ok(format!("{}.lock", credentials_filename(profile)?))
}

/// Advisory lock held for a whole load → refresh → save cycle.
///
/// Refresh tokens are single-use: without this, two concurrent invocations can
/// both read the same stored token, both POST it, and the loser's response is
/// rejected while it overwrites the winner's freshly rotated token. The lock
/// is per profile, so refreshing one profile never blocks another. Released on
/// drop (and by the OS if the process dies).
///
/// Uses `std::fs::File`'s own advisory locking (stable since Rust 1.89), which
/// is `flock` on Unix and `LockFileEx` on Windows.
struct CredentialLock {
    file: std::fs::File,
}

impl CredentialLock {
    fn acquire(profile: Option<&str>) -> Result<Self> {
        Self::at(&config_dir()?.join(lock_filename(profile)?))
    }

    /// Block until this lock file is ours.
    fn at(path: &Path) -> Result<Self> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts
            .open(path)
            .with_context(|| format!("Failed to open lock file {}", path.display()))?;
        file.lock()
            .with_context(|| format!("Failed to lock {}", path.display()))?;
        Ok(Self { file })
    }
}

impl Drop for CredentialLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Reads stored credentials as they are on disk — no lock, no expiry check,
/// no refresh. `load_fresh_credentials` is what you want before *using* a
/// token; this is for looking at one.
pub fn load_credentials(profile: Option<&str>) -> Option<Credentials> {
    let path = credentials_path(profile).ok()?;
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_credentials(creds: &Credentials, profile: Option<&str>) -> Result<()> {
    let path = credentials_path(profile)?;
    write_private(&path, &serde_json::to_string_pretty(creds)?)
        .with_context(|| format!("Failed to write credentials to {}", path.display()))
}

fn token_expires_at(token: &str) -> Option<u64> {
    let payload_b64 = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    json["exp"].as_u64()
}

/// The account a Mapbox token was issued for, from its `u` claim.
///
/// Same payload decode as [`token_expires_at`]. `None` for anything that
/// isn't a readable Mapbox token — this only ever feeds a diagnostic, so a
/// token we can't parse is simply one we say nothing about.
pub fn token_account(token: &str) -> Option<String> {
    let payload_b64 = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    json["u"].as_str().map(str::to_owned)
}

/// Environment variables carrying a token, in resolution order.
///
/// `MAPBOX_ACCESS_TOKEN` is the one clap binds `--token` to. `MapboxAccessToken`
/// is a legacy alias the Tilesets CLI also reads: it has to be checked here
/// too, or setting `MAPBOX_ACCESS_TOKEN` ourselves would silently outrank a
/// token the user had already provided under the other name.
const TOKEN_ENV_VARS: [&str; 2] = ["MAPBOX_ACCESS_TOKEN", "MapboxAccessToken"];

/// The variable clap binds `--token` to, and so the only one of
/// [`TOKEN_ENV_VARS`] that reaches a generic service command. Named here and
/// used by `main.rs` rather than repeated as a literal, so the two cannot
/// drift: `auth whoami` reports this variable as the one in play, and would
/// be wrong the moment the arg bound a different one.
pub const CLAP_TOKEN_ENV: &str = TOKEN_ENV_VARS[0];

/// The token typed on the command line, as distinct from one clap picked up
/// from `MAPBOX_ACCESS_TOKEN` via the `token` arg's `.env()` fallback.
///
/// `get_one` cannot tell the two apart, and they rank differently: a typed
/// flag outranks everything, an environment value does not.
pub fn typed_token(matches: &clap::ArgMatches) -> Option<String> {
    (matches.value_source("token") == Some(clap::parser::ValueSource::CommandLine))
        .then(|| matches.get_one::<String>("token").cloned())
        .flatten()
}

/// The token the environment already provides, with the variable it came from.
pub fn environment_token() -> Option<(&'static str, String)> {
    TOKEN_ENV_VARS.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| (*name, value))
    })
}

/// The warning to print when an environment token shadows a different
/// account's stored login, or `None` when there's nothing to say.
///
/// Environment beats stored credentials throughout this CLI, matching what
/// every comparable tool does — but a token exported once in a shell profile
/// then shadows `mapbox auth login` indefinitely, and an API asked for another
/// account's data with a valid token tends to answer `Not found` rather than
/// anything resembling an auth error. Keep the precedence; drop the silence.
///
/// Pure, so the decision is testable without a real config dir or environment.
/// `remedy` is the literal invocation to suggest. It differs per command
/// because `mapbox`'s globals are positional for the `tilesets-cli` proxy:
/// everything after that subcommand name is forwarded to `tilesets`, so
/// `--use-login` only acts on us if it comes before it.
pub fn shadowed_login_warning(
    env_var: &str,
    env_token: &str,
    logged_in_as: Option<&str>,
    remedy: &str,
) -> Option<String> {
    let logged_in_as = logged_in_as?;
    let env_account = token_account(env_token)?;

    (env_account != logged_in_as).then(|| {
        format!(
            "Warning: {env_var} is a token for account `{env_account}`, but you are \
             logged in as `{logged_in_as}`. The environment takes precedence — run \
             `{remedy}` (or unset {env_var}) to use your login instead."
        )
    })
}

/// Prints [`shadowed_login_warning`] to stderr when it applies.
pub fn warn_if_environment_token_shadows_login(profile: Option<&str>, remedy: &str) {
    let Some((env_var, env_token)) = environment_token() else {
        return;
    };
    let stored = load_credentials(profile);
    let logged_in_as = stored.as_ref().and_then(|c| c.username.as_deref());

    if let Some(warning) = shadowed_login_warning(env_var, &env_token, logged_in_as, remedy) {
        eprintln!("{warning}");
    }
}

fn token_needs_refresh(token: &str) -> bool {
    match token_expires_at(token) {
        Some(exp) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            // Refresh if expiring within 5 minutes
            exp <= now + 300
        }
        // No exp claim (e.g. sk/pk tokens) — never needs refresh
        None => false,
    }
}

fn refresh_credentials(creds: &mut Credentials, debug: bool) -> Result<()> {
    let refresh_token = creds.refresh_token.as_deref().ok_or_else(|| {
        anyhow!("No refresh token stored. Run `mapbox auth login` to re-authenticate.")
    })?;
    let client_id = creds.client_id.as_deref().ok_or_else(|| {
        anyhow!("No client_id stored. Run `mapbox auth login` to re-authenticate.")
    })?;

    let client = crate::http::client()?;
    let params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];

    if debug {
        eprintln!("[debug] POST {} (grant_type=refresh_token)", TOKEN_ENDPOINT);
    }

    let resp = client
        .post(TOKEN_ENDPOINT)
        .form(&params)
        .send()
        .context("Token refresh request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(anyhow!(
            "Token refresh failed ({}): {}\nRun `mapbox auth login` to re-authenticate.",
            status,
            text
        ));
    }

    let json: serde_json::Value = resp.json().context("Invalid JSON from token endpoint")?;

    creds.access_token = json["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No access_token in refresh response"))?
        .to_string();

    // Rotation: server may issue a new refresh token
    if let Some(rt) = json["refresh_token"].as_str() {
        creds.refresh_token = Some(rt.to_string());
    }

    Ok(())
}

/// Force a token refresh regardless of expiry, for `mapbox auth refresh`.
pub fn force_refresh(debug: bool, profile: Option<&str>, mode: Mode) -> Result<()> {
    let _lock = CredentialLock::acquire(profile)?;

    let mut creds = load_credentials(profile)
        .ok_or_else(|| anyhow!("Not currently logged in. Run `mapbox auth login` first."))?;

    refresh_credentials(&mut creds, debug)?;
    save_credentials(&creds, profile)?;

    let expires_at = token_expires_at(&creds.access_token);
    let text = match expires_at {
        Some(exp) => format!(
            "Token refreshed. The new token expires {}.",
            time_until(exp, now())
        ),
        None => "Token refreshed.".to_string(),
    };
    output::emit(
        mode,
        &text,
        json!({
            "refreshed": true,
            "profile": profile_name(profile),
            "expires_at": expires_at,
        }),
    )
}

/// Seconds since the unix epoch, matching `token_expires_at`'s `exp` claim.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// How long is left, phrased for a person.
///
/// The JSON keeps the raw `expires_at`, which is the right answer for a
/// program and the wrong one for a reader: nobody can tell at a glance
/// whether 1789234567 is soon. Relative rather than a wall-clock date
/// because the useful question about an expiry is how much time is left —
/// and because a date would mean pulling in a date library for one line.
///
/// Takes `now` rather than reading the clock so the phrasing can be tested.
fn time_until(expires_at: u64, now: u64) -> String {
    // Reachable through clock skew, even straight after a refresh.
    let Some(remaining) = expires_at.checked_sub(now).filter(|left| *left > 0) else {
        return "immediately — check this machine's clock".to_string();
    };

    let minutes = remaining / 60;
    let hours = minutes / 60;

    match (hours, minutes % 60) {
        (0, 0) => format!("in {remaining} seconds"),
        (0, m) => format!("in {m} minutes"),
        (h, 0) => format!("in {h} hours"),
        (h, m) => format!("in {h} hours {m} minutes"),
    }
}

/// Attaches credential advice to a 401, and leaves every other error alone.
///
/// "Run `mapbox auth login`" is only the right answer sometimes, so the
/// advice is written from where the token that just failed actually came
/// from — which is why `remedy::for_http` leaves a 401's `fix` empty for
/// this to fill: it is the one status whose answer is not a property of the
/// request. A stale token in the environment outranks a stored login, so
/// logging in again will not fix it; a token typed with `--token` outranks
/// both, so neither `--use-login` nor a fresh login touches it.
pub fn with_auth_fix(
    err: anyhow::Error,
    matches: &clap::ArgMatches,
    use_login: bool,
    profile: Option<&str>,
) -> anyhow::Error {
    match err.downcast::<CliError>() {
        Ok(cli) if cli.status == Some(401) => cli
            .with_remedy(credential_remedy(matches, use_login, profile))
            .into(),
        Ok(cli) => cli.into(),
        Err(other) => other,
    }
}

/// The advice for a credential the API refused: the prose, one command to
/// run, and where tokens are documented.
///
/// One helper for the two places that need it — a 401 from a generated
/// command, and a rejected verdict from `--verify` — so they cannot drift
/// into two different answers to one question. The probing is here and the
/// wording is in [`remedy_for`], which takes the facts, so every branch is
/// reachable from a test without an environment or a credential store.
fn credential_remedy(matches: &clap::ArgMatches, use_login: bool, profile: Option<&str>) -> Remedy {
    // The same three candidates in the same order the service arm applies
    // and `whoami` reports — through `resolve_source`, so there is one copy
    // of that order and not a third.
    let flag = typed_token(matches);
    let environment = flag
        .is_none()
        .then(|| matches.get_one::<String>("token").cloned())
        .flatten();
    let stored = load_credentials(profile);

    let source = resolve_source(
        flag.as_deref(),
        environment.as_deref(),
        stored.as_ref().map(|c| c.access_token.as_str()),
        use_login,
    )
    .map(|(source, _)| source);

    remedy_for(source, CLAP_TOKEN_ENV, stored.is_some())
}

/// The advice for each place a token can come from.
///
/// Takes the facts rather than probing for them, the same reason
/// `Mode::resolve` is handed `stdout_is_terminal`.
///
/// `next_actions` carries a command the caller can actually run; `--use-login`
/// and `unset` are shell-level moves and stay in the prose. `whoami` is the
/// action wherever the failing token is one that outranks something else —
/// it is the only thing that says which of the candidates wins — and the
/// login itself wherever a login is what is missing or refused.
fn remedy_for(source: Option<TokenSource>, environment: &str, stored: bool) -> Remedy {
    let action = match source {
        Some(TokenSource::Flag) => "mapbox auth whoami",
        Some(TokenSource::Environment) if stored => "mapbox auth whoami",
        _ => "mapbox auth login",
    };

    Remedy::default()
        .with_fix(&fix_for(source, environment, stored))
        .with_action(Some(action.to_string()))
        .with_doc(Some(remedy::TOKENS_DOC))
}

/// Why the token that just failed was the one in play, and what to do about
/// that particular one.
fn fix_for(source: Option<TokenSource>, environment: &str, stored: bool) -> String {
    match source {
        // A typed flag outranks the environment and the login alike, so the
        // two moves that answer every other branch — `--use-login`, a fresh
        // login — both re-send the same token. Neither is named here even to
        // rule it out: a flag that appears in a `fix` gets tried, and
        // `a_typed_token_is_not_blamed_on_the_environment` keeps it out.
        Some(TokenSource::Flag) => format!(
            "The token was passed with `--token`, which outranks both {environment} and \
             your login — so signing in again would change nothing. Check the token you \
             passed, or drop the flag to use one of the others."
        ),
        Some(TokenSource::Environment) if stored => format!(
            "The token came from {environment}, which outranks your login. Run the same \
             command with `--use-login`, or unset {environment}."
        ),
        Some(TokenSource::Environment) => format!(
            "The token came from {environment}. Run `mapbox auth login` to sign in with an \
             account instead."
        ),
        Some(TokenSource::Login) => {
            "The stored credentials were rejected. Run `mapbox auth login` to sign in again."
                .to_string()
        }
        // A 401 with no token resolved at all: the endpoint refused an
        // anonymous request.
        None => "Run `mapbox auth login` to sign in.".to_string(),
    }
}

/// Load stored credentials, refreshing the access token if it's expiring soon.
/// Returns None if no credentials are stored.
pub fn load_fresh_credentials(debug: bool, profile: Option<&str>) -> Option<Credentials> {
    // Held across load → check → refresh → save: another invocation must not
    // spend the same single-use refresh token concurrently. Taken before the
    // first read so the credentials we test for expiry are the ones we refresh.
    let _lock = match CredentialLock::acquire(profile) {
        Ok(lock) => lock,
        Err(e) => {
            match e.downcast_ref::<DirectoryBlocked>() {
                // Not a locking problem, and saying so would send the reader
                // looking in the wrong place.
                Some(blocked) => eprintln!("Warning: {}", blocked.one_line()),
                None => eprintln!("Warning: could not lock credentials — {e}"),
            }
            return load_credentials(profile);
        }
    };

    let mut creds = load_credentials(profile)?;

    if token_needs_refresh(&creds.access_token) {
        match refresh_credentials(&mut creds, debug) {
            Ok(()) => {
                let _ = save_credentials(&creds, profile);
            }
            Err(e) => {
                eprintln!("Warning: token refresh failed — {e}");
            }
        }
    }

    Some(creds)
}

pub fn logout(profile: Option<&str>, mode: Mode) -> Result<()> {
    let path = credentials_path(profile)?;
    let had_credentials = path.exists();
    if had_credentials {
        std::fs::remove_file(&path)?;
    }

    let text = if had_credentials {
        "Logged out successfully."
    } else {
        "Not currently logged in."
    };
    output::emit(
        mode,
        text,
        json!({ "logged_out": had_credentials, "profile": profile_name(profile) }),
    )
}

/// Where the token the next command will use comes from.
///
/// The order is the generic service arm's in `main.rs`, and has to stay that
/// way: a status command reporting a precedence other than the one actually
/// applied is worse than no status command at all.
/// `pub(crate)` for [`crate::generate_skills`], which keys the precedence it
/// documents off these variants so the prose and the resolver cannot drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenSource {
    /// `--token`, typed on the command line.
    Flag,
    /// The environment, under [`CLAP_TOKEN_ENV`].
    Environment,
    /// Stored credentials from `mapbox auth login`.
    Login,
}

impl TokenSource {
    /// The name a program reads. The prose form is built in
    /// [`Identity::source_prose`], where it can name the variable or profile
    /// the bare word leaves out.
    fn as_str(self) -> &'static str {
        match self {
            TokenSource::Flag => "flag",
            TokenSource::Environment => "environment",
            TokenSource::Login => "login",
        }
    }
}

/// Which of the three candidate tokens wins.
///
/// Takes its candidates rather than probing for them, so every branch is
/// reachable from a test without an environment or a credential store — the
/// same reason [`fix_for`] and [`shadowed_login_warning`] do — and it is
/// what [`credential_remedy`] consults rather than guessing from whether the
/// environment happens to hold a token.
pub(crate) fn resolve_source<'a>(
    flag: Option<&'a str>,
    environment: Option<&'a str>,
    stored: Option<&'a str>,
    use_login: bool,
) -> Option<(TokenSource, &'a str)> {
    if let Some(token) = flag {
        return Some((TokenSource::Flag, token));
    }
    // The step `--use-login` exists to skip. A typed flag still outranks the
    // login above it: that is the one candidate nobody sets by accident, and
    // the service arm treats it the same way.
    if !use_login {
        if let Some(token) = environment {
            return Some((TokenSource::Environment, token));
        }
    }
    stored.map(|token| (TokenSource::Login, token))
}

/// A token's kind, from its prefix: `pk` public, `sk` secret, `tk` temporary.
///
/// Read off the prefix rather than the payload because that is where the API
/// itself reports it — and because the prefix is the one part of a token that
/// is safe to print.
fn token_usage(token: &str) -> Option<&str> {
    let (usage, rest) = token.split_once('.')?;
    (!rest.is_empty() && matches!(usage, "pk" | "sk" | "tk")).then_some(usage)
}

/// Says that `MapboxAccessToken` is set but reaches only the Tilesets proxy.
///
/// The alias is real — `tilesets_cli` reads it because the Tilesets CLI does —
/// but clap binds `--token` to [`CLAP_TOKEN_ENV`] alone. So a token exported
/// under the old name and nothing else authenticates `mapbox tilesets-cli` as
/// one account while every other command uses a stored login as another, and
/// neither half of that is visible from either. Silent only when
/// [`CLAP_TOKEN_ENV`] is set too: then both halves read the same variable and
/// there is no divergence to warn about.
fn legacy_alias_note(legacy_set: bool, clap_saw_environment: bool, winner: &str) -> Option<String> {
    (legacy_set && !clap_saw_environment).then(|| {
        format!(
            "Note: {} is set, but only `mapbox tilesets-cli` reads it — every other \
             command uses the {winner} above. Export {} instead for one answer \
             everywhere.",
            TOKEN_ENV_VARS[1], CLAP_TOKEN_ENV,
        )
    })
}

/// The resolved answer, ready to render either way.
struct Identity<'a> {
    source: TokenSource,
    account: Option<String>,
    usage: Option<&'a str>,
    expires_at: Option<u64>,
    /// The account in the stored credentials, whether or not it won. Carried
    /// so a login the environment is shadowing appears in the report itself,
    /// not only in the warning printed beside it.
    stored_login: Option<&'a str>,
    profile: &'a str,
    /// The endpoint's verdict under `--verify`, or `None` when it was not
    /// asked for. A check that ran and failed never gets here — it leaves as
    /// an error instead.
    verified: Option<Value>,
}

impl Identity<'_> {
    /// Takes `now` rather than reading the clock, so the expiry phrasing is
    /// testable — as [`time_until`] does, for the same reason.
    fn text(&self, now: u64) -> String {
        let mut lines = vec![
            format!(
                "Account:  {}",
                self.account
                    .as_deref()
                    .unwrap_or("unknown (the token carries no account claim)")
            ),
            format!("Source:   {}", self.source_prose()),
            format!("Token:    {}", self.token_prose(now)),
        ];

        // Only worth its own line when it is not the answer already given
        // above: a stored login is either what is in use, or the thing being
        // shadowed.
        if self.source != TokenSource::Login {
            if let Some(stored) = self.stored_login {
                lines.push(format!(
                    "Login:    `{stored}` stored under profile `{}`, not in use",
                    self.profile
                ));
            }
        }

        if let Some(verified) = &self.verified {
            lines.push(format!(
                "Verified: {}",
                verified
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("checked against the Mapbox API")
            ));
        }

        lines.join("\n")
    }

    fn source_prose(&self) -> String {
        match self.source {
            TokenSource::Flag => "--token, typed on the command line".to_string(),
            TokenSource::Environment => format!("{CLAP_TOKEN_ENV} (environment)"),
            TokenSource::Login => format!("mapbox auth login (profile `{}`)", self.profile),
        }
    }

    fn token_prose(&self, now: u64) -> String {
        let usage = match self.usage {
            Some("pk") => "public (pk)",
            Some("sk") => "secret (sk)",
            Some("tk") => "temporary (tk)",
            _ => "unrecognised prefix",
        };
        match self.expires_at {
            Some(exp) => format!("{usage}, expires {}", time_until(exp, now)),
            // `pk` and `sk` tokens carry no `exp` claim at all; only the
            // temporary ones `auth login` issues do.
            None => format!("{usage}, no expiry"),
        }
    }

    fn json(&self) -> Value {
        json!({
            "account": self.account,
            "source": self.source.as_str(),
            // Named only when it is the answer, so a reader of the JSON never
            // has to decide whether a variable that is merely set was used.
            "env_var": (self.source == TokenSource::Environment).then_some(CLAP_TOKEN_ENV),
            "usage": self.usage,
            "expires_at": self.expires_at,
            "profile": self.profile,
            "stored_login": self.stored_login,
            "verified": self.verified,
        })
    }
}

/// Nothing to report, which for a status command is an answer — but a failing
/// one, so `mapbox auth whoami && ...` means what it looks like. A
/// [`CliError`] rather than plain `anyhow` because a script branches on
/// `not_authenticated`, not on the prose.
fn nothing_to_report(use_login: bool, profile: Option<&str>) -> anyhow::Error {
    let fix = if use_login {
        // The environment may well hold a token. `--use-login` asked for it to
        // be ignored, so naming it here would read as a contradiction.
        format!(
            "--use-login was given, and no credentials are stored for profile `{}`. \
             Run `mapbox auth login` first.",
            profile_name(profile)
        )
    } else {
        format!("Run `mapbox auth login`, export {CLAP_TOKEN_ENV}, or pass `--token`.")
    };

    CliError::new("not_authenticated", "No Mapbox token available.")
        .with_remedy(
            Remedy::default()
                .with_fix(&fix)
                .with_action(Some("mapbox auth login".to_string()))
                .with_doc(Some(remedy::TOKENS_DOC)),
        )
        .into()
}

/// Checks a token against `GET /tokens/v2`, the one question a local decode
/// cannot answer.
///
/// A revoked token still carries a readable `u` claim and a future `exp`, so
/// everything else this command reports would look fine. The endpoint always
/// answers `200` and puts the verdict in `code` — which means a non-2xx from
/// here is a transport or platform problem, never a bad token, and has to be
/// reported as one.
fn verify_token(token: &str, debug: bool, timeout: Option<Duration>) -> Result<Value> {
    if debug {
        // Never the real query string. `--debug` output ends up in CI logs and
        // pasted issue reports, and the token is a query parameter here.
        eprintln!("[debug] GET {VALIDATION_ENDPOINT}?access_token=<redacted>");
    }

    let response = crate::http::client()?
        .get(VALIDATION_ENDPOINT)
        .query(&[("access_token", token)])
        // The one request `auth` makes to a Mapbox API rather than to the
        // authorization server, so it is the one `--timeout` has to reach.
        // The other three — refresh, registration, code exchange — carry a
        // few hundred bytes each and keep the client's own budget.
        .timeout(crate::http::budget(timeout, crate::http::Payload::Bounded))
        .send()
        // `reqwest::Error`'s `Display` appends the URL, and the token is in it.
        .map_err(|e| crate::executor::transport_failure("Token check failed", e))?;

    let status = response.status();
    let body = response
        .text()
        .map_err(|e| crate::executor::transport_failure("Failed to read the token check", e))?;

    if !status.is_success() {
        return Err(CliError::http(status.as_u16(), &body).into());
    }

    serde_json::from_str(&body).context("Invalid JSON from the token check endpoint")
}

/// A verdict from [`verify_token`] that is not `TokenValid`.
///
/// Each code becomes its own error code: telling a revoked token from a typo
/// is the whole reason to have run the check, and a caller should not have to
/// parse prose for it. The remedy comes from [`credential_remedy`], which
/// already answers "the token that just failed came from here, so do this".
fn rejected_token(
    code: &str,
    matches: &clap::ArgMatches,
    use_login: bool,
    profile: Option<&str>,
) -> anyhow::Error {
    let (error_code, message) = match code {
        "TokenMalformed" => (
            "token_malformed",
            "The token Mapbox would use is not a well-formed Mapbox token.".to_string(),
        ),
        "TokenInvalid" => (
            "token_invalid",
            "Mapbox does not recognise the token it would be given.".to_string(),
        ),
        "TokenExpired" => (
            "token_expired",
            "The token Mapbox would use has expired.".to_string(),
        ),
        "TokenRevoked" => (
            "token_revoked",
            "The token Mapbox would use has been revoked.".to_string(),
        ),
        // The endpoint is free to grow a sixth verdict, and inventing prose
        // for one nobody here has seen would be worse than quoting it.
        other => (
            "token_rejected",
            format!("Mapbox rejected the token it would be given: `{other}`."),
        ),
    };

    CliError::new(error_code, message)
        .with_remedy(credential_remedy(matches, use_login, profile))
        .into()
}

/// `mapbox auth whoami` — which token the next command will use, and whose.
///
/// Read-only on purpose: [`load_credentials`], not
/// [`load_fresh_credentials`], so asking who you are cannot spend the
/// single-use refresh token or block on another invocation's lock. A stored
/// token already inside the five-minute refresh window is therefore reported
/// as expiring, which is the truth — the next real command is what refreshes
/// it.
///
/// Takes the top-level `matches` because `--token` is global and the two
/// things it can mean rank differently; see [`typed_token`].
pub fn whoami(
    matches: &clap::ArgMatches,
    verify: bool,
    use_login: bool,
    debug: bool,
    profile: Option<&str>,
    mode: Mode,
) -> Result<()> {
    let flag = typed_token(matches);
    // `get_one` folds in the `CLAP_TOKEN_ENV` fallback declared on the arg and
    // cannot say which of the two it returned. Subtracting the typed value
    // leaves exactly what the environment supplied — the same distinction
    // `typed_token` draws, from the other side.
    let environment = flag
        .is_none()
        .then(|| matches.get_one::<String>("token").cloned())
        .flatten();

    let stored = load_credentials(profile);
    let stored_login = stored.as_ref().and_then(|c| c.username.as_deref());

    let Some((source, token)) = resolve_source(
        flag.as_deref(),
        environment.as_deref(),
        stored.as_ref().map(|c| c.access_token.as_str()),
        use_login,
    ) else {
        return Err(nothing_to_report(use_login, profile));
    };

    let verified = verify
        .then(|| verify_token(token, debug, crate::http::requested(matches)))
        .transpose()?;
    if let Some(result) = &verified {
        let code = result
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("no verdict");
        if code != "TokenValid" {
            return Err(rejected_token(code, matches, use_login, profile));
        }
    }

    // The one diagnostic that cannot be a field, because it is about the gap
    // between two of them. Reused rather than rephrased so the CLI has a
    // single wording of this warning.
    if source == TokenSource::Environment {
        if let Some(warning) = shadowed_login_warning(
            CLAP_TOKEN_ENV,
            token,
            stored_login,
            "mapbox --use-login <command>",
        ) {
            eprintln!("{warning}");
        }
    }

    if let Some(note) = legacy_alias_note(
        std::env::var_os(TOKEN_ENV_VARS[1]).is_some_and(|value| !value.is_empty()),
        source == TokenSource::Environment,
        source.as_str(),
    ) {
        eprintln!("{note}");
    }

    let identity = Identity {
        source,
        account: token_account(token),
        usage: token_usage(token),
        expires_at: token_expires_at(token),
        stored_login,
        profile: profile_name(profile),
        verified,
    };

    output::emit(mode, &identity.text(now()), identity.json())
}

/// The profile a result should report. `--profile` is optional everywhere;
/// naming the implicit one keeps the JSON shape the same either way.
fn profile_name(profile: Option<&str>) -> &str {
    profile.unwrap_or("default")
}

/// What an `auth` command would do, for `--dry-run`.
///
/// One function rather than three, so the three answers keep one shape: the
/// profile, the file that would change, and a sentence about the change.
///
/// It reads the store with [`load_credentials`] and never
/// [`load_fresh_credentials`]. A dry run that spent the single-use refresh
/// token in order to report that it would spend the refresh token is the
/// exact mutation the flag exists to avoid — and it takes no
/// [`CredentialLock`] either, since it writes nothing there is anything to
/// serialise against.
///
/// A precondition the real command would fail on is still a failure here:
/// `refresh` with nothing stored reports what `force_refresh` reports rather
/// than describing a plan that could not run. Answering "would refresh" to a
/// command that cannot is worse than not asking.
pub fn describe_plan(action: &str, profile: Option<&str>, mode: Mode) -> Result<()> {
    let path = credentials_path(profile)?;
    let shown = path.display().to_string();
    let name = profile_name(profile);
    let stored = load_credentials(profile);

    let (text, detail) = match action {
        "login" => {
            let replacing = if stored.is_some() {
                format!(", replacing the credentials already at {shown}")
            } else {
                format!(" to {shown}")
            };
            (
                format!(
                    "Dry run — nothing was changed.\n\
                     Would register an OAuth client, open {AUTHORIZATION_ENDPOINT} in a \
                     browser, exchange the code at {TOKEN_ENDPOINT}, and write the \
                     credentials for profile `{name}`{replacing}."
                ),
                json!({
                    "would_replace_existing": stored.is_some(),
                    "authorization_endpoint": AUTHORIZATION_ENDPOINT,
                    "token_endpoint": TOKEN_ENDPOINT,
                    "scopes": requested_scopes().split(' ').map(str::to_string).collect::<Vec<String>>(),
                }),
            )
        }
        "logout" => (
            match stored {
                Some(_) => format!("Dry run — nothing was changed.\nWould delete {shown}."),
                None => format!(
                    "Dry run — nothing was changed.\n\
                     Nothing to delete: no credentials are stored for profile `{name}` \
                     at {shown}."
                ),
            },
            json!({ "would_delete": stored.is_some() }),
        ),
        "refresh" => {
            let creds = stored.ok_or_else(|| {
                anyhow!("Not currently logged in. Run `mapbox auth login` first.")
            })?;
            // The two fields `refresh_credentials` needs. Checking them here
            // is what makes this a real rehearsal: a store written before
            // refresh-token support has neither, and the caller should hear
            // that now rather than after the flag has told them it is fine.
            if creds.refresh_token.is_none() {
                return Err(anyhow!(
                    "No refresh token stored. Run `mapbox auth login` to re-authenticate."
                ));
            }
            if creds.client_id.is_none() {
                return Err(anyhow!(
                    "No client_id stored. Run `mapbox auth login` to re-authenticate."
                ));
            }

            let expires_at = token_expires_at(&creds.access_token);
            let expiry = match expires_at {
                Some(exp) => format!(" The token it replaces expires {}.", time_until(exp, now())),
                None => String::new(),
            };
            (
                format!(
                    "Dry run — nothing was changed.\n\
                     Would POST a refresh_token grant to {TOKEN_ENDPOINT} and rewrite \
                     {shown}.{expiry}"
                ),
                json!({
                    "token_endpoint": TOKEN_ENDPOINT,
                    "expires_at": expires_at,
                }),
            )
        }
        other => unreachable!("`auth` has no `{other}` subcommand"),
    };

    let mut json = json!({
        "dry_run": true,
        "command": format!("auth {action}"),
        "profile": name,
        "credentials_path": shown,
    });
    // Merged rather than nested: the per-action fields are as much a part of
    // the plan as the profile is, and a caller reading `.would_delete` should
    // not have to know which of two levels it landed on.
    if let (Some(object), Some(extra)) = (json.as_object_mut(), detail.as_object()) {
        object.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    }

    output::emit(mode, &text, json)
}

fn register_client(redirect_uri: &str, debug: bool, scopes: &str) -> Result<ClientRegistration> {
    let client = crate::http::client()?;
    let body = serde_json::json!({
        "client_name": "Mapbox CLI",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "scope": scopes,
    });

    // The registration endpoint only honors `scope` from the query string as the
    // ceiling for this client's future authorize/token requests — a `scope` field in
    // the JSON body alone is silently ignored and the ceiling falls back to a
    // conservative read-only default. Send both: query for the ceiling, body for
    // clients/proxies that only look at the JSON payload.
    if debug {
        eprintln!(
            "[debug] POST {}?scope={}",
            REGISTRATION_ENDPOINT,
            percent_encode(scopes)
        );
        eprintln!("[debug] body: {}", body);
    }

    let resp = client
        .post(REGISTRATION_ENDPOINT)
        .query(&[("scope", scopes)])
        .json(&body)
        .send()
        .context("Failed to reach Mapbox OAuth registration endpoint")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(anyhow!("Client registration failed ({}): {}", status, text));
    }

    let json: serde_json::Value = resp
        .json()
        .context("Invalid JSON from registration endpoint")?;
    let client_id = json["client_id"]
        .as_str()
        .ok_or_else(|| anyhow!("No client_id in registration response:\n{}", json))?
        .to_string();
    let client_secret = json["client_secret"].as_str().map(String::from);

    Ok(ClientRegistration {
        client_id,
        client_secret,
    })
}

fn find_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("failed to bind to get a free port")
        .local_addr()
        .unwrap()
        .port()
}

/// A wait in the units whoever reads it is thinking in.
///
/// [`CALLBACK_TIMEOUT`] is minutes and the tests pass milliseconds, and
/// "0 minutes" in a test failure explains nothing.
fn describe(wait: Duration) -> String {
    let seconds = wait.as_secs();
    match seconds {
        0 => format!("{} ms", wait.as_millis()),
        1 => "1 second".to_string(),
        2..=119 => format!("{seconds} seconds"),
        // Only whole minutes get called minutes: 150s as "2 minutes" would
        // overstate a budget someone is timing against.
        _ if seconds.is_multiple_of(60) => format!("{} minutes", seconds / 60),
        _ => format!("{seconds} seconds"),
    }
}

fn generate_pkce() -> (String, String) {
    let verifier_bytes: [u8; 32] = rand::thread_rng().gen();
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Waits for the browser's redirect, for at most `timeout`.
///
/// `TcpListener::accept` has no timeout, so the wait is built out of a
/// non-blocking listener and a poll. The alternative — a thread that abandons
/// the blocked `accept` — leaks the thread and the bound port for the rest of
/// the process, which matters here because the port is in a redirect URI a
/// registered OAuth client is pinned to.
///
/// The timeout is a parameter rather than reading [`CALLBACK_TIMEOUT`]
/// directly so the tests can exercise the poll in milliseconds instead of
/// minutes. `login` passes the constant.
fn wait_for_callback(port: u16, expected_state: &str, timeout: Duration) -> Result<String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .with_context(|| format!("Failed to bind callback listener on port {}", port))?;
    listener
        .set_nonblocking(true)
        .context("Failed to set the callback listener non-blocking")?;

    let deadline = Instant::now() + timeout;
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(accepted) => break accepted,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(CliError::new(
                        "login_timed_out",
                        format!(
                            "No response from the browser after {} — login abandoned.",
                            describe(timeout)
                        ),
                    )
                    .with_remedy(
                        Remedy::default()
                            .with_fix(
                                "Run `mapbox auth login` again, or set \
                                 MAPBOX_ACCESS_TOKEN instead if this is a script or a \
                                 CI job.",
                            )
                            // The retry is a command; exporting a variable is
                            // a shell move, and stays in the prose.
                            .with_action(Some("mapbox auth login".to_string()))
                            .with_doc(Some(remedy::TOKENS_DOC)),
                    )
                    .into());
                }
                // Long enough to cost nothing over five minutes, short enough
                // that the browser's round trip is not what the user waits on.
                std::thread::sleep(Duration::from_millis(50));
            }
            // A connection queued and then reset before `accept` saw it —
            // browser connection pre-warming, a proxy, an AV probe. Nothing
            // about the login has failed, and it may have minutes left, so
            // keep polling rather than abandoning it.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionAborted => continue,
            Err(e) => return Err(anyhow!("Failed to accept OAuth callback: {}", e)),
        }
    };
    // On macOS and the BSDs an accepted socket inherits O_NONBLOCK from its
    // listener, so without this the read below returns `WouldBlock` instead of
    // the request — a login that fails on macOS and passes on Linux.
    stream
        .set_nonblocking(false)
        .context("Failed to set the callback connection blocking")?;
    stream
        .set_read_timeout(Some(CALLBACK_READ_TIMEOUT))
        .context("Failed to set a read timeout on the callback connection")?;

    let mut buf = vec![0u8; 8192];
    let n = stream
        .read(&mut buf)
        .context("Failed to read callback request")?;
    let request = String::from_utf8_lossy(&buf[..n]);

    let html = "<html><body style='font-family:sans-serif;padding:2em'>\
        <h1>Login successful!</h1>\
        <p>You can close this tab and return to the terminal.</p>\
        </body></html>";
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            html.len(),
            html
        )
        .as_bytes(),
    );

    // The `state` checks come first, ahead of the `error` branch, and that
    // ordering is the security property rather than a tidiness preference.
    // `error_description` is attacker-chosen text interpolated into a message
    // that reaches a terminal unescaped, and *anyone* who can open this port
    // can send one — including a page open in the user's browser during the
    // login window, via `fetch` that never needs to read the response. Behind
    // the state check, only the real authorization server's text is printed.
    //
    // RFC 6749 §4.1.2.1 requires the server to echo `state` on an error
    // response too, so a genuine denial still passes. The cost if it ever does
    // not: someone who clicks "deny" is told the state was missing instead of
    // being told they denied it, which the message tries to cover.
    let returned_state = extract_query_param(&request, "state").ok_or_else(|| {
        anyhow!(
            "OAuth callback carried no state — possible CSRF, or an \
             authorization response that did not echo it"
        )
    })?;
    if returned_state != expected_state {
        return Err(anyhow!(
            "State mismatch in OAuth callback — possible CSRF attack"
        ));
    }

    if let Some(err) = extract_query_param(&request, "error") {
        let desc = extract_query_param(&request, "error_description")
            .unwrap_or_else(|| "unknown error".into());
        // Neutralised even behind the state check: this is still text from
        // somewhere else, printed raw under `text`, and the belt-and-braces
        // costs one allocation on a path that is about to end the process.
        return Err(anyhow!(
            "Authorization denied: {} — {}",
            printable(&err),
            printable(&desc)
        ));
    }

    extract_query_param(&request, "code")
        .ok_or_else(|| anyhow!("No authorization code in OAuth callback"))
}

fn extract_query_param(http_request: &str, key: &str) -> Option<String> {
    let path = http_request.lines().next()?.split_whitespace().nth(1)?;
    let query = path.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
            if k == key {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(b as char);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(' ');
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Text with control characters neutralised, for a message that reaches a
/// terminal unescaped.
///
/// [`percent_decode`] maps every decoded byte to the matching scalar, so `%1B`
/// arrives as a real ESC and `%0A` as a newline. Under `-o text` — what `auto`
/// gives a terminal, and therefore what an interactive login runs in —
/// `output::emit_error` prints the message with `eprintln!` and no escaping,
/// so those bytes would be *interpreted*: cursor movement, a cleared line, a
/// forged "Logged in as …" written over the real failure. Under `json`,
/// `serde_json` escapes them to `\u001b`, which is why only one mode was ever
/// exposed.
///
/// Replaced rather than dropped, so a message that had something removed still
/// says so.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            }
        })
        .collect()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn exchange_code_for_token(
    code: &str,
    client_id: &str,
    client_secret: Option<&str>,
    redirect_uri: &str,
    code_verifier: &str,
    debug: bool,
) -> Result<Credentials> {
    let client = crate::http::client()?;

    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];
    // owned storage so the reference stays valid for the lifetime of params
    let secret_owned = client_secret.unwrap_or("").to_string();
    if client_secret.is_some() {
        params.push(("client_secret", &secret_owned));
    }

    if debug {
        eprintln!(
            "[debug] POST {} (grant_type=authorization_code)",
            TOKEN_ENDPOINT
        );
    }

    let resp = client
        .post(TOKEN_ENDPOINT)
        .form(&params)
        .send()
        .context("Token exchange request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(anyhow!("Token exchange failed ({}): {}", status, text));
    }

    let json: serde_json::Value = resp.json().context("Invalid JSON from token endpoint")?;

    let access_token = json["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No access_token in token response:\n{}", json))?
        .to_string();
    let refresh_token = json["refresh_token"].as_str().map(String::from);
    let username = extract_username_from_token(&access_token);

    Ok(Credentials {
        access_token,
        refresh_token,
        username,
        client_id: None,
    })
}

fn extract_username_from_token(token: &str) -> Option<String> {
    // Mapbox tokens are JWTs: base64url(header).base64url(payload).sig
    let payload_b64 = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    // Mapbox JWT payload uses "u" for username
    json["u"].as_str().map(String::from)
}

/// Whether anyone could be watching this run.
///
/// Either stream is enough, and for different reasons. stderr is where the
/// authorization URL and the "if the browser does not open automatically"
/// fallback are printed, so a terminal there means the URL is readable. stdin
/// says nothing about reading, but a terminal on it means this process was
/// started from a session that still has one — `mapbox auth login > log 2>&1`
/// on a desktop, where `open::that` puts the page in front of the user and
/// nothing has to be read at all.
///
/// Neither one is the case that matters: CI, a container without a tty, an
/// agent holding both pipes. There, a browser opens for nobody and the process
/// waits out [`CALLBACK_TIMEOUT`] before failing.
fn someone_could_be_watching() -> bool {
    std::io::stdin().is_terminal() || std::io::stderr().is_terminal()
}

/// Whether the login can still be completed, once the browser has been tried.
///
/// A terminal on stderr means the URL was printed somewhere it can be read and
/// pasted, so whether a browser opened does not matter. With stderr redirected
/// the browser is the only route left — [`someone_could_be_watching`] admitted
/// this run on stdin alone for exactly that reason — so a browser that did not
/// open means nobody can finish, and waiting out [`CALLBACK_TIMEOUT`] only
/// delays saying so.
///
/// The honest limit: the URL has to exist before it can be opened, and it
/// carries the `client_id`, so by the time this is known one client
/// registration has already been spent. Refusing here saves the five minutes,
/// not the registration.
fn login_can_be_completed(stderr_is_terminal: bool, browser_opened: bool) -> bool {
    stderr_is_terminal || browser_opened
}

/// The browser did not open and the URL went somewhere nobody is reading.
///
/// Same code as [`login_needs_a_terminal`] on purpose: a caller branching on
/// it is asking "can this environment log in at all", and the answer is the
/// same no. `ssh` to a headless box and `mapbox auth login > log 2>&1` is the
/// shape that lands here.
fn login_has_no_way_to_show_the_url() -> anyhow::Error {
    CliError::new(
        "interactive_required",
        "No browser could be opened, and stderr is redirected, so the login URL \
         reached nobody.",
    )
    // No `next_actions`: both ways out are changes to how the command is
    // invoked — a terminal on stderr, or a token in the environment — and
    // neither is a line this CLI can hand back to be run. Running the same
    // login again would fail in exactly the same way.
    .with_remedy(
        Remedy::default()
            .with_fix(
                "Run it with stderr on a terminal to get the URL, or set \
                 MAPBOX_ACCESS_TOKEN for a script or a CI job.",
            )
            .with_doc(Some(remedy::TOKENS_DOC)),
    )
    .into()
}

/// Why `login` will not start without a terminal.
///
/// Deliberately **not** overridable by `--yes`. It was, and that reintroduced
/// the failure this check exists to prevent: `MAPBOX_YES=1` is exactly what a
/// CI job exports so its deletes do not block, and it was silently opting
/// `auth login` back into the browser flow — one dynamic client registration
/// left behind and five minutes of wall clock burned per run, for a login that
/// could never complete. A flag about confirmations has no business asserting
/// that a human is present.
///
/// So the answer for a headless caller is a token, and the fix says so. A
/// login on a machine with no terminal at all wants the device authorization
/// grant, which is a feature, not an escape hatch on this one.
fn login_needs_a_terminal() -> anyhow::Error {
    CliError::new(
        "interactive_required",
        "`mapbox auth login` needs a browser and someone to use it, and this run \
         has no terminal on stdin or stderr.",
    )
    .with_remedy(
        Remedy::default()
            .with_fix("Set MAPBOX_ACCESS_TOKEN for a script or a CI job.")
            .with_doc(Some(remedy::TOKENS_DOC)),
    )
    .into()
}

pub fn login(debug: bool, profile: Option<&str>, mode: Mode) -> Result<()> {
    validate_profile(profile)?;
    // Ahead of everything else, `config_dir` included: this is the one failure
    // that costs nothing to find, and refusing after `register_client` would
    // leave a registered OAuth client behind on every CI run that tried.
    if !someone_could_be_watching() {
        return Err(login_needs_a_terminal());
    }
    // Resolve the store up front so an unusable path fails here rather than in
    // `save_credentials` at the very end. Nothing else in this function touches
    // it, so without this the user spends a browser round-trip and a token
    // exchange before finding out the credentials have nowhere to go.
    config_dir()?;

    let port = find_free_port();
    let redirect_uri = format!("http://localhost:{}/callback", port);

    // Computed once: register_client's ceiling and the authorize scope must agree.
    let scopes = requested_scopes();

    output::progress("Registering OAuth client with Mapbox...");
    let registration = register_client(&redirect_uri, debug, &scopes)?;

    let (code_verifier, code_challenge) = generate_pkce();
    let state: String = URL_SAFE_NO_PAD.encode(rand::thread_rng().gen::<[u8; 16]>());

    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        AUTHORIZATION_ENDPOINT,
        percent_encode(&registration.client_id),
        percent_encode(&redirect_uri),
        percent_encode(&scopes),
        state,
        code_challenge,
    );

    output::progress("Opening Mapbox login in your browser...");
    output::progress(&format!(
        "If the browser does not open automatically, visit:\n\n  {auth_url}\n"
    ));

    // Not discarded: with stderr redirected this is the only thing carrying
    // the run, so its failure is the difference between refusing now and
    // stalling for five minutes. See `login_can_be_completed`.
    let browser_opened = open::that(&auth_url).is_ok();
    if !login_can_be_completed(std::io::stderr().is_terminal(), browser_opened) {
        return Err(login_has_no_way_to_show_the_url());
    }

    output::progress(&format!(
        "Waiting for authorization (listening on port {port})..."
    ));
    let code = wait_for_callback(port, &state, CALLBACK_TIMEOUT)?;

    output::progress("Exchanging authorization code for access token...");
    let mut creds = exchange_code_for_token(
        &code,
        &registration.client_id,
        registration.client_secret.as_deref(),
        &redirect_uri,
        &code_verifier,
        debug,
    )?;
    creds.client_id = Some(registration.client_id.clone());

    save_credentials(&creds, profile)?;

    let profile_note = match profile {
        Some(name) if name != "default" => format!(" (profile: {name})"),
        _ => String::new(),
    };
    let text = match &creds.username {
        Some(u) => format!(
            "Logged in as {u}{profile_note}.\n\
             Tip: export MAPBOX_USERNAME={u} to skip --username on each command."
        ),
        None => format!("Logged in successfully{profile_note}."),
    };
    output::emit(
        mode,
        &text,
        json!({
            "logged_in": true,
            "username": creds.username,
            "profile": profile_name(profile),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        fix_for, remedy_for, scopes_with, time_until, with_auth_fix, TokenSource, DEFAULT_SCOPES,
    };

    #[test]
    fn a_flags_scopes_ride_along_only_while_it_is_gated_on() {
        let without = scopes_with(&[]);
        let off: Vec<&str> = without.split(' ').collect();
        assert!(!off.contains(&"statistics:read"), "{off:?}");
        assert_eq!(off, DEFAULT_SCOPES.split(' ').collect::<Vec<&str>>());

        let scopes = scopes_with(&[&["statistics:read"]]);
        let on: Vec<&str> = scopes.split(' ').collect();
        assert!(on.contains(&"statistics:read"), "{on:?}");
        for scope in DEFAULT_SCOPES.split(' ') {
            assert!(on.contains(&scope), "lost {scope} when a flag gated on");
        }
    }

    /// Goes through `feature_flags::flags::ALL`, unlike `scopes_with`
    /// above, so a flag whose scopes never reach `ALL` fails here too.
    #[test]
    fn requested_scopes_is_never_narrower_than_the_default_set() {
        let requested = super::requested_scopes();
        let requested: Vec<&str> = requested.split(' ').collect();
        for scope in DEFAULT_SCOPES.split(' ') {
            assert!(requested.contains(&scope), "lost {scope}");
        }
    }

    /// Every scope the CLI's live operations need, that Mapbox will actually
    /// grant. The two the specs ask for and the platform still does not have
    /// — `fonts:metadata`, `tokens:write` — are deliberately absent; their
    /// operations are filtered out of the command surface instead
    /// (`UNSUPPORTED_OPERATIONS` in `spec.rs`). `fonts:list` and
    /// `fonts:write` moved out of that group and into `needed` once they
    /// became registrable (2026-09-08).
    #[test]
    fn the_requested_scopes_cover_what_the_commands_need() {
        let requested: Vec<&str> = DEFAULT_SCOPES.split(' ').collect();

        for needed in [
            "styles:read",
            "styles:write",
            "styles:list",
            "styles:tiles",
            "fonts:read",
            // `fonts list-fonts`, `upload-font`, `delete-font` and
            // `update-font-metadata` — unblocked once these scopes became registrable.
            "fonts:list",
            "fonts:write",
            "datasets:read",
            "datasets:write",
            "tokens:read",
            // `accounts list-scopes`, which 403'd until this was added.
            "scopes:list",
            "tilesets:read",
            "tilesets:write",
            "tilesets:list",
        ] {
            assert!(requested.contains(&needed), "{needed} is not requested");
        }

        for unavailable in [
            "tokens:write",
            "fonts:metadata",
            "styles:download",
            // Registrable, and deliberately not asked for: the only command
            // that needed it unlocks a style for deletion, and is withheld.
            // A scope no command uses is a capability handed out for free.
            "styles:protect",
        ] {
            assert!(
                !requested.contains(&unavailable),
                "{unavailable} is not registrable; requesting it is silently dropped"
            );
        }
    }

    /// The scope list travels to the registration endpoint twice — as a query
    /// parameter and in the body — and the query half is encoded by hand,
    /// because that request is made before there is a client to hand it to.
    /// A space that survived would end the query value early and register a
    /// client holding whatever prefix got through.
    #[test]
    fn the_scope_query_is_encoded_by_hand_so_it_has_to_be_exact() {
        let encoded = percent_encode(DEFAULT_SCOPES);

        assert!(
            !encoded.contains(' '),
            "a raw space would truncate the query value: {encoded}"
        );
        assert_eq!(
            encoded.matches("%20").count(),
            DEFAULT_SCOPES.split(' ').count() - 1,
            "every separator has to survive as one: {encoded}"
        );
        assert!(encoded.contains("styles%3Atiles"), "{encoded}");
        assert!(encoded.contains("user-feedback%3Aread"), "{encoded}");
    }

    /// Per byte rather than per character, upper-case hex, and the unreserved
    /// set passing through untouched — the three things RFC 3986 asks for and
    /// the endpoint quietly depends on.
    #[test]
    fn percent_encoding_spares_the_unreserved_set_and_nothing_else() {
        assert_eq!(percent_encode("aZ09-_.~"), "aZ09-_.~");
        assert_eq!(percent_encode("a b/c?d=e&f"), "a%20b%2Fc%3Fd%3De%26f");
        assert_eq!(percent_encode("é"), "%C3%A9");
        assert_eq!(percent_encode(""), "");
    }

    use crate::output::CliError;

    const ENV: &str = "MAPBOX_ACCESS_TOKEN";

    /// Re-running `auth login` does nothing about a stale environment
    /// variable, so every place a token can come from has to say something
    /// different.
    #[test]
    fn the_advice_follows_where_the_token_came_from() {
        let env_and_login = fix_for(Some(TokenSource::Environment), ENV, true);
        assert!(env_and_login.contains("--use-login"), "{env_and_login}");
        assert!(env_and_login.contains(ENV), "{env_and_login}");

        let env_only = fix_for(Some(TokenSource::Environment), ENV, false);
        assert!(env_only.contains(ENV), "{env_only}");
        assert!(env_only.contains("auth login"), "{env_only}");
        assert!(!env_only.contains("--use-login"), "{env_only}");

        let login_only = fix_for(Some(TokenSource::Login), ENV, true);
        assert!(login_only.contains("rejected"), "{login_only}");
        assert!(login_only.contains("auth login"), "{login_only}");

        let anonymous = fix_for(None, ENV, false);
        assert_eq!(anonymous, "Run `mapbox auth login` to sign in.");
    }

    /// The branch this used to get wrong, and the reason it mattered: the
    /// advice was written from whether the environment *held* a token rather
    /// than from which token was *used*, so a `--token` that failed was
    /// blamed on `MAPBOX_ACCESS_TOKEN` and answered with `--use-login` — a
    /// flag that re-sends the same typed token, producing the identical 401
    /// and the identical advice. Anything a caller follows has to change
    /// which token goes out.
    #[test]
    fn a_typed_token_is_not_blamed_on_the_environment() {
        let typed = fix_for(Some(TokenSource::Flag), ENV, true);

        assert!(typed.contains("--token"), "{typed}");
        assert!(
            !typed.contains("Run `mapbox auth login`"),
            "a login cannot outrank a typed flag: {typed}"
        );
        assert!(
            !typed.contains("--use-login"),
            "`--use-login` re-sends the same typed token — following this \
             would loop: {typed}"
        );
    }

    /// `next_actions` is for a command that runs, so it is `whoami` wherever
    /// the question is "which of these tokens won" and the login wherever a
    /// login is what is missing or refused.
    #[test]
    fn the_suggested_command_matches_the_question() {
        let action = |source, stored| {
            remedy_for(source, ENV, stored)
                .next_actions
                .first()
                .cloned()
                .expect("a command to run")
        };

        assert_eq!(action(Some(TokenSource::Flag), true), "mapbox auth whoami");
        assert_eq!(action(Some(TokenSource::Flag), false), "mapbox auth whoami");
        assert_eq!(
            action(Some(TokenSource::Environment), true),
            "mapbox auth whoami"
        );
        assert_eq!(
            action(Some(TokenSource::Environment), false),
            "mapbox auth login"
        );
        assert_eq!(action(Some(TokenSource::Login), true), "mapbox auth login");
        assert_eq!(action(None, false), "mapbox auth login");
    }

    /// The credential advice belongs to a 401 and to nothing else. A 403
    /// carries its own — the token was accepted there, so "sign in again"
    /// would be the wrong answer — and `remedy` writes that one.
    /// A parse with `token` declared, which is what `typed_token` asks clap
    /// about — `value_source` panics on an argument that was never declared.
    fn line(args: &[&str]) -> clap::ArgMatches {
        clap::Command::new("mapbox")
            .arg(clap::Arg::new("token").long("token"))
            .try_get_matches_from(args)
            .expect("fixture parses")
    }

    #[test]
    fn only_a_401_earns_the_credential_advice() {
        let fixed = with_auth_fix(
            CliError::http(401, "{}").into(),
            &line(&["mapbox"]),
            false,
            None,
        );
        let cli = fixed.downcast_ref::<CliError>().expect("still a CliError");
        assert!(cli.fix.is_some());
        assert!(
            cli.next_actions
                .iter()
                .any(|a| a.starts_with("mapbox auth")),
            "the advice has to name a command to run: {:?}",
            cli.next_actions
        );

        for status in [403, 404, 500] {
            let untouched = with_auth_fix(
                CliError::http(status, "{}").into(),
                &line(&["mapbox"]),
                false,
                None,
            );
            assert!(
                untouched
                    .downcast_ref::<CliError>()
                    .expect("still a CliError")
                    .fix
                    .is_none(),
                "HTTP {status} should not carry credential advice"
            );
        }
    }

    #[test]
    fn a_plain_error_passes_through_unchanged() {
        let err = with_auth_fix(
            anyhow!("something else went wrong"),
            &line(&["mapbox"]),
            false,
            None,
        );
        assert!(err.downcast_ref::<CliError>().is_none());
        assert_eq!(err.to_string(), "something else went wrong");
    }

    #[test]
    fn an_expiry_is_phrased_as_time_remaining() {
        assert_eq!(time_until(1_000_045, 1_000_000), "in 45 seconds");
        assert_eq!(time_until(1_003_540, 1_000_000), "in 59 minutes");
        assert_eq!(time_until(1_007_200, 1_000_000), "in 2 hours");
        assert_eq!(time_until(1_009_000, 1_000_000), "in 2 hours 30 minutes");
    }

    #[test]
    fn an_expiry_already_past_says_so_rather_than_going_negative() {
        assert!(time_until(999_000, 1_000_000).starts_with("immediately"));
        assert!(time_until(1_000_000, 1_000_000).starts_with("immediately"));
    }

    /// A Mapbox token is `<prefix>.<base64url payload>.<signature>`; only the
    /// payload's `u` claim matters here.
    fn token_for_account(account: &str) -> String {
        let payload = URL_SAFE_NO_PAD.encode(format!(r#"{{"u":"{account}"}}"#));
        format!("pk.{payload}.signature")
    }

    #[test]
    fn token_account_reads_the_u_claim() {
        assert_eq!(
            token_account(&token_for_account("someone")).as_deref(),
            Some("someone")
        );
    }

    #[test]
    fn a_shadowed_login_is_reported_with_both_accounts() {
        let warning = shadowed_login_warning(
            "MAPBOX_ACCESS_TOKEN",
            &token_for_account("other-account"),
            Some("me"),
            "mapbox --use-login ...",
        )
        .expect("a mismatch should warn");

        assert!(warning.contains("other-account"), "{warning}");
        assert!(warning.contains("me"), "{warning}");
        assert!(warning.contains("MAPBOX_ACCESS_TOKEN"), "{warning}");
    }

    /// The variable is named in the warning because it is the thing to unset,
    /// and the legacy spelling is the one people forget they have.
    #[test]
    fn the_warning_names_whichever_variable_is_set() {
        let warning = shadowed_login_warning(
            "MapboxAccessToken",
            &token_for_account("other"),
            Some("me"),
            "mapbox --use-login ...",
        )
        .expect("a mismatch should warn");

        assert!(warning.contains("MapboxAccessToken"), "{warning}");
    }

    #[test]
    fn matching_accounts_are_not_worth_mentioning() {
        assert!(shadowed_login_warning(
            "MAPBOX_ACCESS_TOKEN",
            &token_for_account("me"),
            Some("me"),
            "mapbox --use-login ..."
        )
        .is_none());
    }

    /// Both are diagnostics-only inputs: with nothing to compare, saying
    /// something speculative is worse than saying nothing.
    #[test]
    fn nothing_is_claimed_when_there_is_nothing_to_compare() {
        assert!(
            shadowed_login_warning(
                "MAPBOX_ACCESS_TOKEN",
                &token_for_account("other"),
                None,
                "mapbox --use-login ...",
            )
            .is_none(),
            "no stored login to shadow"
        );
        assert!(
            shadowed_login_warning(
                "MAPBOX_ACCESS_TOKEN",
                "not-a-mapbox-token",
                Some("me"),
                "mapbox --use-login ...",
            )
            .is_none(),
            "an unreadable token's account is unknown, not different"
        );
    }

    /// The order in [`resolve_source`] is the service arm's in `main.rs`. If
    /// one moves and the other does not, this command starts lying about
    /// which token is in play — worse than not offering it at all.
    #[test]
    fn the_reported_precedence_is_the_one_that_gets_applied() {
        // A typed flag outranks everything, `--use-login` included: that flag
        // only ever asked for the environment to be skipped.
        assert_eq!(
            resolve_source(Some("tk.flag"), Some("tk.env"), Some("tk.stored"), false),
            Some((TokenSource::Flag, "tk.flag"))
        );
        assert_eq!(
            resolve_source(Some("tk.flag"), Some("tk.env"), Some("tk.stored"), true),
            Some((TokenSource::Flag, "tk.flag"))
        );

        // The environment beats a stored login, and only `--use-login`
        // reverses it.
        assert_eq!(
            resolve_source(None, Some("tk.env"), Some("tk.stored"), false),
            Some((TokenSource::Environment, "tk.env"))
        );
        assert_eq!(
            resolve_source(None, Some("tk.env"), Some("tk.stored"), true),
            Some((TokenSource::Login, "tk.stored"))
        );

        assert_eq!(
            resolve_source(None, None, Some("tk.stored"), false),
            Some((TokenSource::Login, "tk.stored"))
        );
        assert_eq!(resolve_source(None, None, None, false), None);
        // `--use-login` with nothing stored has nowhere to fall back to: the
        // environment token is precisely what it refused.
        assert_eq!(resolve_source(None, Some("tk.env"), None, true), None);
    }

    #[test]
    fn a_usage_prefix_is_read_and_never_invented() {
        assert_eq!(token_usage("pk.body.signature"), Some("pk"));
        assert_eq!(token_usage("sk.body.signature"), Some("sk"));
        assert_eq!(token_usage("tk.body.signature"), Some("tk"));
        // Anything else is reported as unrecognised rather than guessed at.
        assert_eq!(token_usage("xx.body.signature"), None);
        assert_eq!(token_usage("pk."), None);
        assert_eq!(token_usage("nodotsatall"), None);
    }

    /// A stored login the environment is outranking has to appear in the
    /// report, not only in the warning beside it: a caller reading the JSON
    /// never sees stderr at all.
    #[test]
    fn a_shadowed_login_shows_up_in_both_renderings() {
        let identity = Identity {
            source: TokenSource::Environment,
            account: Some("env-account".to_string()),
            usage: Some("sk"),
            expires_at: None,
            stored_login: Some("login-account"),
            profile: "default",
            verified: None,
        };

        let text = identity.text(1_000);
        assert!(text.contains("Account:  env-account"), "{text}");
        assert!(text.contains(CLAP_TOKEN_ENV), "{text}");
        assert!(
            text.contains("`login-account` stored under profile `default`, not in use"),
            "{text}"
        );
        assert!(text.contains("secret (sk), no expiry"), "{text}");

        let json = identity.json();
        assert_eq!(json["account"], "env-account");
        assert_eq!(json["source"], "environment");
        assert_eq!(json["env_var"], CLAP_TOKEN_ENV);
        assert_eq!(json["stored_login"], "login-account");
        assert_eq!(
            json["verified"],
            Value::Null,
            "null is what distinguishes a check nobody asked for from one that passed"
        );
    }

    /// The login *is* the answer here, so repeating it as something being
    /// shadowed would print one fact twice.
    #[test]
    fn a_login_in_use_is_not_also_reported_as_shadowed() {
        let identity = Identity {
            source: TokenSource::Login,
            account: Some("me".to_string()),
            usage: Some("tk"),
            expires_at: Some(1_000 + 3_600),
            stored_login: Some("me"),
            profile: "work",
            verified: Some(json!({ "code": "TokenValid" })),
        };

        let text = identity.text(1_000);
        assert!(!text.contains("not in use"), "{text}");
        assert!(
            text.contains("mapbox auth login (profile `work`)"),
            "the profile is the part that says which login: {text}"
        );
        assert!(
            text.contains("temporary (tk), expires in 1 hours"),
            "{text}"
        );
        assert!(text.contains("Verified: TokenValid"), "{text}");

        // No variable was read, so none is named.
        assert_eq!(identity.json()["env_var"], Value::Null);
    }

    /// Both halves of the alias problem are invisible from either side: the
    /// proxy authenticates as one account and everything else as another,
    /// with nothing on screen to say so.
    #[test]
    fn the_legacy_alias_is_only_worth_a_note_when_it_diverges() {
        let note = legacy_alias_note(true, false, "login").expect("a divergence to report");
        assert!(note.contains(TOKEN_ENV_VARS[1]), "{note}");
        assert!(note.contains("tilesets-cli"), "{note}");
        assert!(note.contains(CLAP_TOKEN_ENV), "{note}");
        assert!(
            note.contains("login"),
            "the note has to name what the other commands use instead: {note}"
        );

        assert!(
            legacy_alias_note(true, true, "environment").is_none(),
            "with both variables set every command reads the same one"
        );
        assert!(legacy_alias_note(false, false, "login").is_none());
    }

    /// `mapbox auth whoami && ...` has to mean what it looks like, and the
    /// advice has to survive `--use-login` — where naming the environment
    /// variable would contradict the flag just given.
    #[test]
    fn nothing_to_report_fails_with_advice_a_script_can_branch_on() {
        let err = nothing_to_report(false, None);
        let cli = err.downcast_ref::<CliError>().expect("a branchable code");
        assert_eq!(cli.code, "not_authenticated");
        let fix = cli.fix.as_deref().unwrap_or_default();
        assert!(fix.contains("mapbox auth login"), "{fix}");
        assert!(fix.contains(CLAP_TOKEN_ENV), "{fix}");
        assert_eq!(cli.next_actions, ["mapbox auth login"]);

        let err = nothing_to_report(true, Some("work"));
        let cli = err.downcast_ref::<CliError>().unwrap();
        let fix = cli.fix.as_deref().unwrap_or_default();
        assert!(fix.contains("`work`"), "{fix}");
        assert!(
            !fix.contains(CLAP_TOKEN_ENV),
            "--use-login refused the environment; offering it back contradicts the flag: {fix}"
        );
    }

    /// Telling a revoked token from a typo is the whole reason to spend a
    /// round-trip on `--verify`, so the distinction lives in the code and not
    /// only in the prose.
    #[test]
    fn every_verdict_gets_its_own_code() {
        for (verdict, expected) in [
            ("TokenMalformed", "token_malformed"),
            ("TokenInvalid", "token_invalid"),
            ("TokenExpired", "token_expired"),
            ("TokenRevoked", "token_revoked"),
        ] {
            let err = rejected_token(verdict, &line(&["mapbox"]), false, None);
            let cli = err.downcast_ref::<CliError>().expect("a branchable code");
            assert_eq!(cli.code, expected);
            assert!(
                cli.fix.is_some(),
                "{verdict} left the reader with no remedy"
            );
        }

        // A verdict this code has never seen is quoted, not paraphrased.
        let err = rejected_token("TokenSomethingNew", &line(&["mapbox"]), false, None);
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, "token_rejected");
        assert!(cli.message.contains("TokenSomethingNew"), "{}", cli.message);
    }

    use super::*;

    /// A port bound and released, so `wait_for_callback` can bind it itself.
    /// Racy in principle, unique enough in practice — the same trade
    /// `find_free_port` makes in production.
    fn a_free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .expect("bind a free port")
            .local_addr()
            .expect("read the bound address")
            .port()
    }

    /// The branch `someone_could_be_watching` opens on stdin alone is carried
    /// entirely by the browser, so a browser that did not open has to end the
    /// run rather than start a five-minute wait.
    ///
    /// `ssh` to a headless box, `mapbox auth login > log 2>&1`: stdin is a
    /// tty, so the run is admitted; `open::that` then finds no browser, and
    /// the URL is in a log nobody is reading.
    #[test]
    fn a_login_nobody_can_see_or_open_does_not_wait() {
        assert!(!login_can_be_completed(false, false));
    }

    /// The other three corners. A readable URL is enough on its own — the user
    /// can paste it — and a browser that opened is enough on its own, which is
    /// the whole point of admitting the stdin-only case.
    #[test]
    fn either_a_readable_url_or_an_opened_browser_is_enough() {
        assert!(login_can_be_completed(true, false));
        assert!(login_can_be_completed(false, true));
        assert!(login_can_be_completed(true, true));
    }

    /// Drives one callback request through `wait_for_callback` and returns
    /// whatever came back, so the tests below differ only by query string.
    fn callback_returning(query: &str) -> Result<String> {
        let port = a_free_port();
        let query = query.to_string();
        let browser = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match std::net::TcpStream::connect(("127.0.0.1", port)) {
                    Ok(stream) => break stream,
                    Err(_) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("never reached the callback listener: {e}"),
                }
            };
            std::thread::sleep(Duration::from_millis(200));
            let request = format!("GET /callback?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            stream.write_all(request.as_bytes()).expect("send callback");
        });

        let outcome = wait_for_callback(port, "the-state", Duration::from_secs(5));
        browser.join().expect("the stand-in browser panicked");
        outcome
    }

    /// The `error` branch used to run *before* the state check, which made it
    /// the one way into this handler that no CSRF check guarded — and it
    /// interpolates attacker-chosen text into a message printed raw to a
    /// terminal. Anyone able to open the port can send one, including a page in
    /// the user's browser during the login window, with no local code
    /// execution at all.
    #[test]
    fn an_error_response_with_the_wrong_state_is_rejected_not_printed() {
        let err = callback_returning(
            // One line on purpose: a literal space here would end the
            // request line and the state would never be seen.
            "error=access_denied&error_description=visit+evil.example&state=wrong",
        )
        .expect_err("a mismatched state cannot succeed");

        let message = err.to_string();
        assert!(
            message.contains("State mismatch"),
            "expected the CSRF refusal, got: {message}"
        );
        assert!(
            !message.contains("evil.example"),
            "unauthenticated text reached the message: {message}"
        );
    }

    /// A denial from the real authorization server still reports itself, which
    /// is what makes putting the state check first affordable — RFC 6749
    /// §4.1.2.1 has the server echo `state` on error responses too.
    #[test]
    fn a_genuine_denial_still_says_so() {
        let err = callback_returning(
            "error=access_denied&error_description=The+user+said+no&state=the-state",
        )
        .expect_err("a denial is not a success");

        let message = err.to_string();
        assert!(message.contains("Authorization denied"), "{message}");
        assert!(message.contains("access_denied"), "{message}");
        assert!(message.contains("The user said no"), "{message}");
    }

    /// And the text is neutralised even there. `%1B` decodes to a real ESC,
    /// and `text` mode prints the message with no escaping, so the sequence
    /// would be interpreted rather than shown.
    #[test]
    fn control_characters_never_reach_the_terminal() {
        let err = callback_returning(
            "error=access_denied&error_description=%1B%5B2KLogged+in%0A&state=the-state",
        )
        .expect_err("a denial is not a success");

        let message = err.to_string();
        assert!(
            !message.contains('\u{1b}'),
            "an escape survived into the message: {message:?}"
        );
        assert!(
            !message.contains('\n'),
            "a newline survived into the message: {message:?}"
        );
        // Neutralised, not dropped, so the reader can see something was there.
        assert!(
            message.contains(char::REPLACEMENT_CHARACTER),
            "the removal left no trace: {message:?}"
        );
    }

    /// Pinned separately from the paths above, because it is the property the
    /// whole reordering rests on.
    #[test]
    fn printable_only_touches_control_characters() {
        assert_eq!(printable("The user said no"), "The user said no");
        assert_eq!(printable("caf\u{e9} — ok"), "caf\u{e9} — ok");
        assert_eq!(printable("a\u{1b}[2Kb"), "a\u{fffd}[2Kb");
        assert_eq!(printable("a\r\nb"), "a\u{fffd}\u{fffd}b");
    }

    /// A duration nobody has to convert in their head, at the boundaries the
    /// three-point test used to skip.
    #[test]
    fn a_wait_is_described_at_its_boundaries() {
        assert_eq!(describe(Duration::from_secs(1)), "1 second");
        assert_eq!(describe(Duration::from_secs(2)), "2 seconds");
        assert_eq!(describe(Duration::from_secs(119)), "119 seconds");
        assert_eq!(describe(Duration::from_secs(120)), "2 minutes");
        // Not "2 minutes": rounding down would overstate a budget someone is
        // timing against.
        assert_eq!(describe(Duration::from_secs(150)), "150 seconds");
    }

    /// A browser that never comes back must not hold the process forever.
    ///
    /// This is the whole reason the poll exists: `accept()` alone blocked
    /// until Ctrl-C, and in CI until the runner killed the job. Asserted on
    /// the code rather than the prose, and on the elapsed time in both
    /// directions — returning instantly would mean the deadline was already
    /// past, which passes an equality check and fixes nothing.
    #[test]
    fn a_callback_that_never_comes_times_out() {
        let started = Instant::now();
        let err = wait_for_callback(a_free_port(), "some-state", Duration::from_millis(300))
            .expect_err("nothing connected, so this cannot succeed");
        let elapsed = started.elapsed();

        assert_eq!(
            err.downcast_ref::<CliError>().map(|e| e.code.as_str()),
            Some("login_timed_out"),
            "{err}"
        );
        assert!(
            elapsed >= Duration::from_millis(300),
            "returned in {elapsed:?}, so it did not actually wait"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "waited {elapsed:?}, so the deadline is not being honoured"
        );
    }

    /// The success path, and with it the platform trap in the middle of it.
    ///
    /// The listener is non-blocking so the poll can time out, and on macOS and
    /// the BSDs the accepted socket inherits that — verified, not assumed — so
    /// without the explicit `set_nonblocking(false)` the read returns
    /// `WouldBlock` and the login fails on those platforms only. Which is the
    /// bug that reaches users after passing CI on Linux.
    ///
    /// The pause between connecting and writing is what makes this catch it.
    /// A stand-in browser that connects and writes in one breath leaves the
    /// request sitting in the socket buffer before the read runs, so a
    /// non-blocking read finds it there and succeeds — and the test passes
    /// with the fix removed. Connect, wait, *then* write, and the read has to
    /// block to see anything.
    #[test]
    fn a_callback_carrying_the_code_is_read_back() {
        let port = a_free_port();
        let browser = std::thread::spawn(move || {
            // The listener is bound by `wait_for_callback`, not here, so retry
            // rather than assume this thread lost the race to start.
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match std::net::TcpStream::connect(("127.0.0.1", port)) {
                    Ok(stream) => break stream,
                    Err(_) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("never reached the callback listener: {e}"),
                }
            };
            std::thread::sleep(Duration::from_millis(200));
            let request = "GET /callback?code=the-code&state=the-state \
                           HTTP/1.1\r\nHost: localhost\r\n\r\n";
            stream.write_all(request.as_bytes()).expect("send callback");
        });

        let code = wait_for_callback(port, "the-state", Duration::from_secs(5))
            .expect("the callback carried a code and a matching state");
        assert_eq!(code, "the-code");
        browser.join().expect("the stand-in browser panicked");
    }

    /// A duration nobody has to convert in their head. Pinned because the
    /// production value is minutes and every test value is milliseconds, and
    /// the message is the only place the two meet.
    #[test]
    fn a_wait_is_described_in_units_a_reader_expects() {
        assert_eq!(describe(Duration::from_millis(300)), "300 ms");
        assert_eq!(describe(Duration::from_secs(30)), "30 seconds");
        assert_eq!(describe(CALLBACK_TIMEOUT), "5 minutes");
    }

    /// A unique scratch dir, so tests never touch the real config dir.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mapbox-cli-test-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_file_where_the_config_directory_belongs_says_so() {
        // The legacy `~/.mapbox` token file: a plain file exactly where the
        // credential directory now goes. `create_dir_all` calls this "File
        // exists", which is true and useless.
        let path = scratch("config-dir-collision").join(".mapbox");
        std::fs::write(&path, "pk.a-legacy-token").unwrap();

        let err = prepare_config_dir(&path).unwrap_err();

        let full = err.to_string();
        assert!(full.contains("is a file"), "{full}");
        assert!(full.contains(&path.display().to_string()), "{full}");
        assert!(
            full.contains("mv "),
            "the message has to carry the fix, not just the diagnosis: {full}"
        );
        assert!(
            full.contains(CONFIG_DIR_ENV),
            "the other way out belongs here too: {full}"
        );

        // The same fact for a command that is not about credentials at all,
        // which only needs to explain why the stored ones went missing.
        let brief = err
            .downcast_ref::<DirectoryBlocked>()
            .expect("the obstruction has to survive as its own type")
            .one_line();
        assert_eq!(brief.lines().count(), 1, "{brief}");
        assert!(brief.contains("not a directory"), "{brief}");
        assert!(
            brief.contains(&format!("mv {}", path.display())),
            "the short form still carries the fix itself: {brief}"
        );
        assert!(
            !brief.contains(CONFIG_DIR_ENV),
            "the secondary route is what the short form drops: {brief}"
        );
    }

    #[test]
    fn the_config_directory_is_created_and_restricted() {
        let path = scratch("config-dir-create").join(".mapbox");
        prepare_config_dir(&path).unwrap();
        assert!(path.is_dir());
        #[cfg(unix)]
        assert_eq!(mode_of(&path), 0o700);

        // Running again over an existing directory is the common case, and
        // must stay quiet.
        prepare_config_dir(&path).unwrap();
    }

    #[test]
    fn rejects_unsafe_profile_names() {
        assert!(validate_profile(None).is_ok());
        assert!(validate_profile(Some("default")).is_ok());
        assert!(validate_profile(Some("android_app")).is_ok());

        for bad in ["", "..", "../evil", "a/b", "a\\b"] {
            assert!(
                validate_profile(Some(bad)).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn default_profile_keeps_the_plain_filename() {
        // The unnamed profile and an explicit `default` must resolve to the
        // same file, and to the historical name.
        assert_eq!(credentials_filename(None).unwrap(), "credentials.json");
        assert_eq!(
            credentials_filename(Some("default")).unwrap(),
            "credentials.json"
        );
        assert_eq!(
            credentials_filename(None).unwrap(),
            credentials_filename(Some("default")).unwrap()
        );
    }

    #[test]
    fn named_profiles_get_their_own_filename() {
        assert_eq!(
            credentials_filename(Some("android_app")).unwrap(),
            "credentials-android_app.json"
        );
        assert_eq!(
            credentials_filename(Some("a")).unwrap(),
            "credentials-a.json"
        );
        // Distinct profiles never collide, and never collide with the default.
        assert_ne!(
            credentials_filename(Some("a")).unwrap(),
            credentials_filename(Some("b")).unwrap()
        );
        assert_ne!(
            credentials_filename(Some("a")).unwrap(),
            credentials_filename(None).unwrap()
        );
        // Validation still applies on this path.
        assert!(credentials_filename(Some("../evil")).is_err());
    }

    #[test]
    fn lock_filename_is_per_profile() {
        assert_eq!(lock_filename(None).unwrap(), "credentials.json.lock");
        assert_eq!(
            lock_filename(Some("android_app")).unwrap(),
            "credentials-android_app.json.lock"
        );
        assert_ne!(
            lock_filename(Some("a")).unwrap(),
            lock_filename(Some("b")).unwrap()
        );
    }

    /// Proves the create-time mode, not a chmod applied afterwards: the mode is
    /// read straight after `open(2)` returns, before `write_private` would run
    /// any `set_permissions`.
    #[cfg(unix)]
    #[test]
    fn file_is_owner_only_at_creation_time() {
        let dir = scratch("create");
        let path = dir.join("credentials.json");

        let file = create_private_file(&path).unwrap();
        let mode = mode_of(&path);
        drop(file);

        // The umask can only clear bits, so assert the security property
        // (no group/other access) rather than an exact mode.
        assert_eq!(
            mode & 0o077,
            0,
            "group/other bits set at creation: {mode:o}"
        );
        assert_eq!(mode & 0o777 & !0o600, 0, "unexpected bits: {mode:o}");

        // create_new: a second create must fail rather than reuse the file.
        assert!(create_private_file(&path).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_ends_up_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("overwrite");
        let path = dir.join("credentials-android_app.json");

        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&path), 0o644);

        write_private(&path, r#"{"access_token":"x"}"#).unwrap();

        // The rename swaps in a fresh inode, so the replaced file's 0644 is
        // discarded rather than inherited.
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"access_token":"x"}"#
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_leaves_no_temp_files_behind() {
        let dir = scratch("atomic");
        let path = dir.join("credentials.json");

        write_private(&path, "{}").unwrap();
        write_private(&path, r#"{"access_token":"second"}"#).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "credentials.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"access_token":"second"}"#
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lock_is_exclusive_while_held_and_free_after_drop() {
        let dir = scratch("lock");
        let path = dir.join("credentials.json.lock");

        let contender = || {
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .unwrap()
        };

        let held = CredentialLock::at(&path).unwrap();
        let other = contender();
        assert!(
            other.try_lock().is_err(),
            "a second exclusive lock was granted while the first was held"
        );
        drop(other);

        drop(held);

        let after = contender();
        assert!(after.try_lock().is_ok(), "lock was not released on drop");
        after.unlock().unwrap();
        drop(after);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn harden_dir_restricts_to_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        harden_dir(&dir).unwrap();

        assert_eq!(mode_of(&dir), 0o700);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The rename is the last thing `write_private` does, so a failure there
    /// leaves the temporary file holding exactly the credentials the real file
    /// would have held, at a dotted path nothing else ever cleans up. The
    /// failure is the half worth pinning: a directory sitting where the file
    /// belongs is what a half-migrated `~/.mapbox` looks like, and the token
    /// must not survive it.
    #[test]
    fn a_failed_move_takes_the_temporary_credentials_with_it() {
        let dir = scratch("write-private-rename-failure");
        let occupied = dir.join("credentials.json");
        std::fs::create_dir(&occupied).expect("a directory where the file belongs");

        let err = write_private(&occupied, r#"{"access_token":"tk.secret"}"#)
            .expect_err("a file cannot be renamed onto a directory");

        let message = format!("{err:#}");
        assert!(
            message.contains(&occupied.display().to_string()),
            "the failure has to name where it was going: {message}"
        );

        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "credentials.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "the token was left behind in {leftovers:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
