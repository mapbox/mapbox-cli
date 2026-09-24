use anyhow::{Context, Result};
use serde_yaml::Value;

#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub name: String,
    pub title: String,
    pub description: Option<String>,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone)]
pub struct Operation {
    /// The service this command lives under — which is **not** always the
    /// service of the spec file it was parsed from. See
    /// [`CLI_COMMAND_EXTENSION`].
    pub service: String,
    /// Where the command sits under that service: `["get"]` for a flat
    /// `mapbox tilequery get`, `["draft", "get"]` for a nested
    /// `mapbox styles draft get`. Never empty.
    pub command_path: Vec<String>,
    pub summary: String,
    pub description: Option<String>,
    pub method: String,
    pub path_template: String,
    pub path_params: Vec<Parameter>,
    pub query_params: Vec<Parameter>,
    /// What the spec says this operation's request body may carry, if it
    /// takes one at all.
    pub body: Option<RequestBody>,
    pub base_url: String,
    /// Set when the operation needs a scope that isn't registrable as OAuth
    /// (see `UNSUPPORTED_OPERATIONS` below). A `mapbox auth login` token can
    /// never carry that scope, so the command refuses to run instead of
    /// failing later with a confusing 403.
    pub disabled_scope: Option<&'static str>,
    /// The operation that shows one of the things this one lists, when the
    /// spec describes such a pair. Filled in by [`link_detail_operations`].
    pub detail: Option<DetailOperation>,
    /// The listing this operation's items come from — the reverse of
    /// [`detail`](Self::detail), and where the ids it takes are to be found.
    /// Filled in by the same pass.
    pub listing: Option<ListingOperation>,
    /// The spec's own `deprecated: true` — Mapbox saying the endpoint is on
    /// its way out.
    ///
    /// A different claim from this CLI retiring a command name, and kept
    /// apart from it for that reason: see [`crate::deprecation`], which
    /// turns both into the warning a caller reads.
    pub deprecated: bool,
    /// Set for an operation listed in [`WITHHELD_OPERATIONS`].
    withheld: bool,
    /// Extra, visible names this command answers to — listed in `--help`
    /// and `--schema`. See [`COMMAND_ALIASES`]. Empty except for the one
    /// operation whose spec-generated name is worth keeping alongside a
    /// better one.
    pub aliases: Vec<&'static str>,
    /// Extra names this command answers to but doesn't advertise anywhere
    /// — not `--help`, not `--schema`, not the docs page. Also from
    /// [`COMMAND_ALIASES`]. A caller already using one keeps working. Only
    /// `clap` and this field know it exists, which is why
    /// `api_command_surface`'s fixture reads the built command tree instead
    /// of the schema, which can't see hidden aliases by design.
    ///
    /// `String`, not `&'static str`, because this holds the *generated*
    /// name (`camel_to_kebab` of the spec's `operationId`), computed at
    /// parse time rather than written in the table.
    pub hidden_aliases: Vec<String>,
}

/// (service, operationId, the media type the API actually wants) for
/// operations whose spec gets the content type wrong.
///
/// `starFile`'s spec says `application/json` with a `boolean` schema, but
/// the service rejects that with `400 Must be plaintext true or false`.
/// Sending the same `true` as `text/plain` works (204). Verified with curl
/// against production.
///
/// Kept as a table instead of a branch in the executor, so the fix is
/// visible next to the operation it's for, and removing it later is just
/// deleting a row.
const BODY_CONTENT_TYPE_OVERRIDES: &[(&str, &str, &str)] = &[("styles", "starFile", "text/plain")];

/// (service, parameter name, the `arg_name` to use instead) for a parameter
/// whose spec name is also a global argument's id — `--profile`, `--token`,
/// `--username`, `--id`, `--output`, `--schema`, `--dry-run`, `--yes`,
/// `--timeout`, `--use-login`, `--debug`.
///
/// `directions.yaml`'s path parameter is genuinely named `profile` — that is
/// the API's own name for it, and the path template substitutes on
/// [`Parameter::name`], not `arg_name`, so the spec can't just rename it.
/// But every `clap::Arg` is built from `arg_name`
/// (`build_operation_command`), and clap has one namespace of ids per
/// command: a second `Arg::new("profile")` on the same command silently
/// replaces the global one instead of erring, so the routing profile this
/// parameter means and the credentials profile the global flag means become
/// one and the same id, whichever definition happened to be added last winning
/// the help text while the *other* one's reader (`main.rs`, reading
/// `matches.get_one::<String>("profile")` to pick a credentials file) still
/// runs — `mapbox directions route mapbox/driving …` failed with
/// `Invalid profile name "mapbox/driving"` this way before this table
/// existed. `tests/source_guards.rs`'s `no_generated_flag_shadows_a_global`
/// catches the `--flag`/`-short` half of this; it can't catch a positional,
/// since a positional has neither.
///
/// Kept as a table rather than a branch, for the same reason
/// [`BODY_CONTENT_TYPE_OVERRIDES`] is: the fix sits next to the operation
/// it's for, and outgrowing a global name later is just deleting a row.
const ARG_NAME_OVERRIDES: &[(&str, &str, &str)] = &[
    ("directions", "profile", "routing-profile"),
    ("isochrone", "profile", "routing-profile"),
    ("map-matching", "profile", "routing-profile"),
    ("matrix", "profile", "routing-profile"),
];

/// The `arg_name` a parameter should present as, when its spec name collides
/// with a global argument's id. See [`ARG_NAME_OVERRIDES`].
fn arg_name_override(service_name: &str, param_name: &str) -> Option<&'static str> {
    ARG_NAME_OVERRIDES
        .iter()
        .find(|(svc, name, _)| *svc == service_name && *name == param_name)
        .map(|(_, _, arg_name)| *arg_name)
}

/// The media types an operation's request body may be sent as.
///
/// This used to be a plain `has_body: bool`, which forced the executor to
/// always send `application/json`. That's right for most operations but
/// wrong for three: `uploadSpriteImage` (raw SVG bytes), `batchUploadSprite`
/// (multipart form), and `uploadChunk` (raw bytes). Those three had no way
/// to send a body at all under the old design.
#[derive(Debug, Clone)]
pub struct RequestBody {
    /// Whether the operation refuses to work without a body.
    ///
    /// We read this but don't enforce it: clap still lets you run
    /// `create-style` with no body. Rejecting that locally would change what
    /// the CLI refuses, not just what it describes. This field exists so
    /// `--schema` can correctly say which bodies the API requires, instead
    /// of calling all of them optional.
    pub required: bool,
    /// Declared media types, in the order the spec lists them. Usually one;
    /// `initUpload` declares both `application/octet-stream` and
    /// `application/json`.
    pub content_types: Vec<String>,
    /// The multipart field the files go under — `images` for
    /// `batchUploadSprite`. Read from the schema instead of hard-coded,
    /// since the API matches on this field name exactly; guessing it wrong
    /// would fail the request with no clear error.
    pub multipart_field: Option<String>,
}

pub const MULTIPART: &str = "multipart/form-data";

/// The path placeholders that the global `--username` flag fills, rather
/// than each becoming its own parameter.
///
/// Kept as one shared list because four places need to agree on it:
/// `parse_spec` drops these as parameters, `executor::execute` fills them
/// in, `link_detail_operations` won't treat them as an identifier, and
/// `crate::schema` describes the flag that fills them. If a spec used a new
/// spelling and only some of those four places knew about it, things would
/// break inconsistently. `every_url_placeholder_has_an_argument` fails the
/// build if this list and the schema ever disagree.
pub const ACCOUNT_PLACEHOLDERS: [&str; 3] = ["username", "owner", "account"];

impl RequestBody {
    /// Whether `--data` applies.
    ///
    /// An empty list means the spec declared a `requestBody` without saying
    /// what type it is. The CLI has always assumed JSON in that case, and
    /// still does — the alternative would be removing a flag that works
    /// today.
    pub fn accepts_json(&self) -> bool {
        self.content_types.is_empty() || self.content_types.iter().any(|ct| is_json(ct))
    }

    /// The media type `--file` would send: the first declared type that
    /// isn't JSON. `None` for a JSON-only body — that's why `--file` never
    /// shows up on those commands.
    pub fn file_content_type(&self) -> Option<&str> {
        self.content_types
            .iter()
            .find(|ct| !is_json(ct))
            .map(String::as_str)
    }

    /// Whether the files become form parts rather than the body itself.
    pub fn is_multipart(&self) -> bool {
        self.file_content_type()
            .is_some_and(|ct| essence(ct) == MULTIPART)
    }

    /// The media type for a body that's typed text, not a file.
    ///
    /// `--data` carries it as-is. `starFile`'s whole body is just the word
    /// `true`, so there's no point making that a file.
    pub fn text_content_type(&self) -> Option<&str> {
        self.content_types
            .iter()
            .find(|ct| essence(ct).starts_with("text/"))
            .map(String::as_str)
    }
}

/// The media type an operation really wants, when the spec is wrong.
fn content_type_override(service_name: &str, operation_id: &str) -> Option<&'static str> {
    BODY_CONTENT_TYPE_OVERRIDES
        .iter()
        .find(|(svc, op, _)| *svc == service_name && *op == operation_id)
        .map(|(_, _, content_type)| *content_type)
}

/// A media type without its parameters or casing — `application/json` out of
/// `Application/JSON; charset=utf-8`.
fn essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// `application/json`, and the `+json` structured suffixes with it.
fn is_json(content_type: &str) -> bool {
    let essence = essence(content_type);
    essence == "application/json" || essence.ends_with("+json")
}

/// (service name, operationId, why) for operations this CLI chooses not to
/// expose.
///
/// Different from `UNSUPPORTED_OPERATIONS`, which lists what the platform
/// makes impossible. These operations would work fine — we just chose not
/// to offer them — so the reason has to be written down, or a later reader
/// might "fix" the omission by mistake.
const WITHHELD_OPERATIONS: &[(&str, &str, &str)] = &[
    // "Lock or unlock a style from editing and deletion." Unlocking is the
    // dangerous half: it turns a protected style into a deletable one, and
    // a CLI would make that a single line with no confirmation.
    //
    // Verified 2026-09-08 against production with a real token holding
    // `styles:protect` (already registrable): the endpoint works —
    // `PUT .../protected` returns 200. It rejects a JSON body
    // (`{"protected":false}` → 400 "Must be plaintext true or false") and
    // wants the literal string `true`/`false` instead. So unlike the
    // admin-only endpoints below, this isn't blocked by the platform — we're
    // withholding it purely for the safety reason above.
    (
        "styles",
        "setStyleProtected",
        "unlocks a style for deletion",
    ),
    // Admin-only, gated by role rather than scope. A `mapbox auth login`
    // token gets a bare 403 with no scope named, even for a Mapbox-owned
    // account.
    ("styles", "adminGetStyle", "admin-only endpoint"),
    ("styles", "adminUpdateStyle", "admin-only endpoint"),
    // 3D model assets are a different product from the glyph/metadata
    // endpoints the rest of `fonts` covers, and we've never fetched one —
    // no account we can reach has a model to test against, so shipping this
    // command would mean shipping it untested.
    //
    // Verified 2026-09-08: this is a test-data gap, not a scope problem. A
    // request for a nonexistent model returned 404 "Model ... not found",
    // not 403 — so the endpoint only needs `fonts:read`, which
    // `DEFAULT_SCOPES` already requests.
    ("fonts", "getModelAsset", "not supported yet"),
    // The v1-v3 API is dead to this CLI. `getLegacyTile` answers 410 to
    // every request — the spec agrees, marking it deprecated and no longer
    // supported. `getLegacyGrid` rejects a `mapbox auth login` token with a
    // JSONP-wrapped 401, for every version and tileset, including the one
    // `getGrid` works on. Neither can succeed, so neither is a command.
    (
        "maps",
        "getLegacyGrid",
        "legacy v1-v3 API rejects modern tokens",
    ),
    ("maps", "getLegacyTile", "legacy v1-v3 API is retired"),
];

fn withheld(service_name: &str, operation_id: &str) -> bool {
    WITHHELD_OPERATIONS
        .iter()
        .any(|(svc, op, _)| *svc == service_name && *op == operation_id)
}

/// We build `command_name` from the spec's own `operationId`, and sometimes
/// that gives a command an ugly name — tilequery names its one operation
/// after its URL path instead of what it does. We can't fix the spec, so
/// this table gives that command a better name instead.
///
/// Table shape: `(service, operationId, alias, show_generated_name)`.
///   - `service` / `operationId`: which operation this is about — the same
///     key `WITHHELD_OPERATIONS` and `BODY_CONTENT_TYPE_OVERRIDES` use.
///   - `alias`: the better name.
///   - `show_generated_name`: `true` keeps the generated name as the
///     command and adds the alias as a second, equally visible spelling.
///     `false` swaps them: the alias becomes the command, and the
///     generated name survives only as a hidden alias — it still runs, but
///     nothing publishes it (not `--help`, not `--schema`, not
///     `docs/commands.md`).
///
/// Empty today. Its one row used to rename tilequery's operation to
/// `get-tilequery`; [`CLI_COMMAND_EXTENSION`] now names that operation
/// `tilequery get` directly, making the row redundant. The mechanism stays
/// because the extension can't do the other thing a row can: keep an old
/// spelling working while publishing it nowhere.
const COMMAND_ALIASES: &[(&str, &str, &str, bool)] = &[];

/// The OpenAPI extension that says where an operation's command belongs:
/// `x-mapbox-cli-command: [service, path_segment...]`.
///
/// This is written onto every operation `openapi/` keeps, so `openapi/`
/// carries the full answer and nothing here second-guesses it. Two things
/// to know:
///
///   - **The first element is the real service, and doesn't have to match
///     the spec file it came from.** `styles.yaml`'s sprite operations name
///     `sprites`; the one operation in `vectortiles.yaml` names `tilesets`.
///     So a service in [`MAPBOX_SPEC_ENTRIES`] can end up with zero
///     operations, while a service no file is wired under can end up with
///     five. [`regroup_by_service`] does that regrouping — which is why
///     `parse_spec`'s output is an intermediate step, not the final surface.
///   - **The rest is a path, not a single name.** More than one segment
///     nests: `["draft", "get"]` puts the command under a `draft` group, as
///     `mapbox styles draft get`.
///
/// Not present in `custom-openapi/` files — those keep the older behavior:
/// their file's own service name, and one flat generated command name.
const CLI_COMMAND_EXTENSION: &str = "x-mapbox-cli-command";

/// Where [`CLI_COMMAND_EXTENSION`] says this operation's command goes, as
/// (service, command path). `None` if the operation declares nothing.
///
/// A value that's present but malformed is an error, not something to fall
/// back from. It can only mean a bug in the strip step, and silently
/// falling back to the file's own service and generated name would put the
/// command under the wrong service — exactly what this extension exists to
/// prevent.
fn cli_command_target(
    op: &Value,
    operation_id: Option<&str>,
) -> Result<Option<(String, Vec<String>)>> {
    let declared = &op[CLI_COMMAND_EXTENSION];
    if declared.is_null() {
        return Ok(None);
    }

    let malformed = || {
        anyhow::anyhow!(
            "`{CLI_COMMAND_EXTENSION}` on `{}` is not a [service, path...] list of at least \
             two strings — `openapi/` is vendored and regenerating it is a maintainer-only \
             step; open an issue if you hit this",
            operation_id.unwrap_or("an operation with no operationId"),
        )
    };

    let segments: Vec<String> = declared
        .as_sequence()
        .ok_or_else(malformed)?
        .iter()
        .map(|segment| segment.as_str().map(str::to_string))
        .collect::<Option<_>>()
        .ok_or_else(malformed)?;

    match segments.split_first() {
        Some((service, path)) if !path.is_empty() => Ok(Some((service.clone(), path.to_vec()))),
        _ => Err(malformed()),
    }
}

/// The alias for this operation, if it has one, and whether the generated
/// name it replaces should still show up alongside it.
///
/// A row whose `service` or `operationId` doesn't match anything just
/// returns `None` here, same as a working row that hasn't matched yet — so
/// a typo'd row would go unnoticed from this function alone.
/// `every_command_alias_names_a_real_operation` is the test that checks
/// each row actually reaches a real operation.
fn alias_for(service_name: &str, operation_id: &str) -> Option<(&'static str, bool)> {
    COMMAND_ALIASES
        .iter()
        .find(|(svc, op, _, _)| *svc == service_name && *op == operation_id)
        .map(|(_, _, alias, show_generated)| (*alias, *show_generated))
}

/// What an operation ends up called, combining its generated name with
/// whatever [`COMMAND_ALIASES`] says about it: the command name, its
/// visible aliases, and its hidden ones.
///
/// Split out of `parse_spec` so both branches below can be tested directly,
/// without needing a real row in the table for each — every row in the
/// table today says `false`, so the `true` branch would otherwise never
/// run.
fn command_names(
    generated_name: String,
    alias: Option<(&'static str, bool)>,
) -> (String, Vec<&'static str>, Vec<String>) {
    match alias {
        // The generated name stays the command; the alias is a second
        // spelling that `--help` also lists.
        Some((alias, true)) => (generated_name, vec![alias], vec![]),
        // The alias becomes the command; the generated name still works
        // but is hidden.
        Some((alias, false)) => (alias.to_string(), vec![], vec![generated_name]),
        None => (generated_name, vec![], vec![]),
    }
}

impl Operation {
    /// The last segment of [`command_path`](Self::command_path) — the name
    /// `clap` gives the leaf subcommand.
    pub fn command_name(&self) -> &str {
        self.command_path
            .last()
            .map(String::as_str)
            .expect("a command path is never empty")
    }

    /// The command as it is typed after `mapbox`: `styles draft get`.
    pub fn command(&self) -> String {
        format!("{} {}", self.service, self.command_path.join(" "))
    }

    /// Whether this is a service's own health check, rather than something
    /// a person or an agent would actually want to call.
    ///
    /// Every spec that has one puts it at the service root or a
    /// conventional health-check path, so matching on the path catches all
    /// five of them. Two have no `operationId` at all, so their command
    /// names come from a summary and can't be matched by name.
    ///
    /// Excluded for a different reason than `UNSUPPORTED_OPERATIONS`: these
    /// aren't impossible to call, they're just not this CLI's business.
    /// Three of the five return 404 in production anyway.
    pub fn is_liveness_probe(&self) -> bool {
        matches!(self.path_template.as_str(), "/" | "/mbx-health")
    }

    /// Whether this CLI declines to expose the operation. See
    /// [`WITHHELD_OPERATIONS`].
    pub fn is_withheld(&self) -> bool {
        self.withheld
    }

    /// Whether the operation is part of the command surface at all.
    ///
    /// One shared check instead of three repeated conditions, because two
    /// places must agree exactly: the command tree
    /// (`build_service_command`) and the schema (`crate::schema`). An
    /// operation that's described but not runnable — or runnable but not
    /// described — is worse than one that's neither.
    pub fn is_exposed(&self) -> bool {
        self.disabled_scope.is_none() && !self.is_liveness_probe() && !self.is_withheld()
    }

    /// Whether running this operation changes something on Mapbox's side —
    /// which is what earns it a `--dry-run`.
    ///
    /// We use the HTTP method, and only the HTTP method, deliberately. It's
    /// the one signal every spec carries, so the rule stays correct even
    /// for a service nobody's looked at yet, or the next one added to
    /// `openapi-specs`. A hand-kept list of mutating operation IDs would go
    /// stale on the first sync. The cost: a read-only POST like
    /// `batchGeocode` (posts a query, gets answers back) gets an unneeded
    /// `--dry-run`. That's a harmless spare flag on a few commands, versus
    /// the alternative of a missing one on a real `delete`.
    pub fn is_mutating(&self) -> bool {
        matches!(
            self.method.to_ascii_uppercase().as_str(),
            "POST" | "PUT" | "PATCH" | "DELETE"
        )
    }
}

/// A sibling operation that lists the items another one shows one of.
#[derive(Debug, Clone)]
pub struct ListingOperation {
    /// The whole command minus `mapbox`: `styles list`. Includes the
    /// service, because a pair can straddle two services — operations are
    /// paired by URL path, and `x-mapbox-cli-command` can file either end
    /// of one path under a different service.
    pub command: String,
    /// The listing's own path — the detail operation's path minus its last
    /// segment. Kept here so a caller can see which path parameters the
    /// listing itself needs too (`/styles/v1/{username}/{style_id}/sprite`
    /// still needs a style), instead of re-deriving that elsewhere.
    pub path_template: String,
}

/// A sibling operation that takes one identifier and returns one item.
#[derive(Debug, Clone)]
pub struct DetailOperation {
    /// The whole command minus `mapbox`, as [`ListingOperation::command`].
    pub command: String,
    /// What the identifier is called, for the hint's placeholder.
    pub parameter: String,
}

/// (service name, operationId, scope) for operations whose required scope
/// isn't in the Accounts API's registration allowlist. Audited 2026-08-28.
///
/// Re-check before removing an entry. We can't fix these ourselves — Mapbox
/// has to make the scope registrable — and an entry comes off once a direct
/// `POST /oauth/register` grants it back unchanged.
///
/// `fonts:list` and `fonts:write` came off this list on 2026-09-08 once
/// both became registrable, which is why `listFonts`, `uploadFont`,
/// `deleteFont` and `updateFontMetadata` all ship today (`DEFAULT_SCOPES`
/// in `auth.rs` now requests both scopes). `updateFontMetadata` briefly sat
/// in `WITHHELD_OPERATIONS` instead: a same-day retest found
/// `PATCH .../{face}/metadata` returning 404 "Font not found for expected
/// owner" against a font the same token could otherwise upload, read and
/// delete. A later retest that same day, on a freshly uploaded font, got
/// 200 both ways — most likely the new scope's authorization just hadn't
/// finished propagating yet. Moved back here rather than left in
/// `WITHHELD_OPERATIONS`, since the 404 was a propagation delay, not a real
/// problem with the operation.
const UNSUPPORTED_OPERATIONS: &[(&str, &str, &str)] = &[
    // Confirmed live 2026-09-08: `fonts:metadata` is a real scope name, not
    // a typo in the docs — the endpoint literally answers 403 "This API
    // requires a token with fonts:metadata scope". It's just not in the
    // registration allowlist yet.
    ("fonts", "getFontCoverage", "fonts:metadata"),
    // tokens:write isn't registrable either — confirmed with a direct
    // POST /oauth/register against production, which silently drops it
    // from the granted scope even when requested via both body and query.
    // It only exists in the older, role-gated (ADMIN-only) token-creation
    // path, not this newer one (DCR).
    ("accounts", "createToken", "tokens:write"),
    ("accounts", "updateToken", "tokens:write"),
    ("accounts", "deleteToken", "tokens:write"),
    // Found by testing it, not by reading the spec: the styles spec
    // documents no scope, but an early probe got 403 "requires a token
    // with styles:download scope". `POST /oauth/register` then drops
    // `styles:download` from the granted set, so no login can get it.
    //
    // Deeper problem too (verified 2026-09-08 with a real token, no scope
    // involved): the endpoint now answers 403 "This is a prerelease API.
    // Please contact support at help@mapbox.com to request access." So
    // access is gated per-account, not by OAuth scope at all — making
    // `styles:download` registrable wouldn't unblock this command by
    // itself, which is why it wasn't registered alongside the other two
    // fonts scopes.
    ("styles", "downloadStyleZip", "styles:download"),
];

fn unsupported_scope_for(service_name: &str, operation_id: &str) -> Option<&'static str> {
    UNSUPPORTED_OPERATIONS
        .iter()
        .find(|(svc, op, _)| *svc == service_name && *op == operation_id)
        .map(|(_, _, scope)| *scope)
}

#[derive(Debug, Clone)]
pub struct Parameter {
    pub name: String,
    pub arg_name: String,
    pub required: bool,
    pub description: Option<String>,
    pub enum_values: Vec<String>,
    pub is_boolean: bool,
    /// The spec's `type`, when clap can check it before we spend a request
    /// finding out the value is wrong. No `boolean` here — that's handled
    /// as a flag instead of a value.
    pub numeric: Option<Numeric>,
}

/// A numeric parameter's width. Kept separate from the plain string case,
/// but the value is still sent to the query string as text either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numeric {
    Integer,
    Float,
}

/// One spec file's embedded OpenAPI YAML, with the name this repo files it
/// under, ready for [`parse_spec`].
///
/// `name` isn't necessarily a service anyone can type — that's decided per
/// operation by [`CLI_COMMAND_EXTENSION`] instead. This is just the file's
/// own name: the fallback service for an operation that declares no
/// target, the key that `WITHHELD_OPERATIONS` and similar tables match on,
/// and what a drift check compares those tables against. `maps` is the
/// clearest example — the entry is still called that, even though the
/// service it used to produce is gone.
#[derive(Clone, Copy)]
pub struct SpecEntry {
    pub name: &'static str,
    pub yaml: &'static str,
}

/// Every spec file this CLI ships with. Each `yaml` is pulled in via
/// `include_str!` from `openapi/`, a copy this repo vendors.
///
/// Not what the CLI builds commands from — see [`effective_services`], and
/// [`CLI_COMMAND_EXTENSION`] for why a file's name and a service's name are
/// two different things.
///
/// `maps` and `vectortiles` are listed, but each contributes only one
/// operation, and both now target the merged `tilesets` command group
/// (#116) rather than a group of their own — see [`MERGED_SERVICES`].
pub const MAPBOX_SPEC_ENTRIES: &[SpecEntry] = &[
    SpecEntry {
        name: "accounts",
        yaml: include_str!("../openapi/api-accounts/tokens-api.yaml"),
    },
    SpecEntry {
        name: "fonts",
        yaml: include_str!("../openapi/api-fonts/fonts.production.v1.yaml"),
    },
    SpecEntry {
        name: "geocoder",
        yaml: include_str!("../openapi/api-geocoder/geocoding-v6.production.yaml"),
    },
    SpecEntry {
        name: "maps",
        yaml: include_str!("../openapi/api-rastertiles/rastertiles.production.v1.yaml"),
    },
    SpecEntry {
        name: "static-images",
        yaml: include_str!("../openapi/api-gl/static-images.production.v1.yaml"),
    },
    SpecEntry {
        name: "static-tiles",
        yaml: include_str!("../openapi/api-gl/static-tiles.production.v1.yaml"),
    },
    SpecEntry {
        name: "styles",
        yaml: include_str!("../openapi/api-styles/styles.production.v1.yaml"),
    },
    SpecEntry {
        name: "tilequery",
        yaml: include_str!("../openapi/api-tilequery/tilequery.production.v1.yaml"),
    },
    SpecEntry {
        name: "vectortiles",
        yaml: include_str!("../openapi/api-vectortiles/vectortiles.production.v1.yaml"),
    },
];

/// Services whose spec this repo writes and versions itself, under
/// `custom-openapi/<service>/openapi/<file>.yaml` (one level up, not two —
/// see `include_str!("../custom-openapi/search/openapi/search.yaml")`).
///
/// For an API that `openapi-specs` doesn't publish a usable spec for yet. A
/// name here wins over the same name in [`MAPBOX_SPEC_ENTRIES`]. Delete the
/// override once upstream ships the service — a drift check flags a name
/// wired on both sides, for exactly this reason.
pub const CUSTOM_SPEC_ENTRIES: &[SpecEntry] = &[
    SpecEntry {
        name: "search",
        yaml: include_str!("../custom-openapi/search/openapi/search.yaml"),
    },
    SpecEntry {
        name: "directions",
        yaml: include_str!("../custom-openapi/directions/openapi/directions.yaml"),
    },
    SpecEntry {
        name: "isochrone",
        yaml: include_str!("../custom-openapi/isochrone/openapi/isochrone.yaml"),
    },
    SpecEntry {
        name: "map-matching",
        yaml: include_str!("../custom-openapi/map-matching/openapi/map-matching.yaml"),
    },
    SpecEntry {
        name: "matrix",
        yaml: include_str!("../custom-openapi/matrix/openapi/matrix.yaml"),
    },
];

/// The list the CLI actually generates commands from: [`MAPBOX_SPEC_ENTRIES`],
/// with each [`CUSTOM_SPEC_ENTRIES`] override swapped in and the
/// custom-only services added at the end.
pub fn effective_spec_entries() -> Vec<SpecEntry> {
    merge_entries(MAPBOX_SPEC_ENTRIES, CUSTOM_SPEC_ENTRIES)
}

/// The services the CLI builds its command tree from: every wired spec
/// parsed, then regrouped by each operation's own target service.
///
/// One function because the two steps can't be separated — see
/// [`regroup_by_service`] — and every caller needs both anyway.
pub fn effective_services() -> Result<Vec<ServiceSpec>> {
    let parsed: Vec<ServiceSpec> = effective_spec_entries()
        .iter()
        .map(|entry| parse_spec(entry.name, entry.yaml))
        .collect::<Result<_>>()?;
    Ok(regroup_by_service(parsed))
}

/// Split out so this precedence rule can be tested against small tables.
/// Custom wins by replacing the entry in place — appending instead would
/// leave two entries claiming one service name, which clap only catches in
/// a debug build.
fn merge_entries(mapbox: &[SpecEntry], custom: &[SpecEntry]) -> Vec<SpecEntry> {
    assert_no_duplicate_name(mapbox, "MAPBOX_SPEC_ENTRIES");
    assert_no_duplicate_name(custom, "CUSTOM_SPEC_ENTRIES");

    let mut merged: Vec<SpecEntry> = mapbox
        .iter()
        .map(|entry| {
            custom
                .iter()
                .find(|override_entry| override_entry.name == entry.name)
                .copied()
                .unwrap_or(*entry)
        })
        .collect();

    merged.extend(
        custom
            .iter()
            .filter(|entry| {
                !mapbox
                    .iter()
                    .any(|from_mapbox| from_mapbox.name == entry.name)
            })
            .copied(),
    );

    merged
}

/// Without this, a second entry with the same name would silently
/// disappear — [`merge_entries`]'s `find`/filter just drops it, with no
/// record of which spec got discarded. This catches the duplicate in
/// either table before the merge can hide it.
fn assert_no_duplicate_name(entries: &[SpecEntry], table: &str) {
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        assert!(
            seen.insert(entry.name),
            "{table} lists `{}` more than once",
            entry.name
        );
    }
}

/// Title and description for a service that no spec file is wired under.
///
/// [`CLI_COMMAND_EXTENSION`] builds these services out of operations that
/// actually live in other files — `sprites` from five operations in
/// `styles.yaml`, `tilesets` from one operation each in `rastertiles.yaml`,
/// `rasterarrays.yaml`, `tilequery.yaml` and `vectortiles.yaml` (#116),
/// `static` from one operation each in `static-images.yaml` and
/// `static-tiles.yaml`. None of those files' `info.title` describes the
/// merged service, so this table is hand-written to fill that gap. A
/// service that still owns its own file keeps that file's `info` as-is.
const MERGED_SERVICES: &[(&str, &str, &str)] = &[
    (
        "sprites",
        "Sprites API",
        "The sprite sheet a style draws its icons from, and the individual icons in it.",
    ),
    (
        "tilesets",
        "Tiles API",
        "Raster and vector tiles, by tileset id.",
    ),
    (
        "static",
        "Static API",
        "Static map images and raster tiles rendered from a Mapbox style.",
    ),
];

/// One line of help for an intermediate command group — a path segment
/// several operations share, that shows up as a command in the tree with
/// no operation of its own.
///
/// Hand-written because nothing else describes it: the group exists only
/// because [`CLI_COMMAND_EXTENSION`] filed two commands under one shared
/// word, and no spec says anything about that word. Keyed by the whole
/// path, service included — that's how `crate::build_service_command`
/// looks it up. `every_command_group_is_described` checks this table
/// against the groups the surface actually builds.
const COMMAND_GROUPS: &[(&str, &str)] = &[(
    "styles draft",
    "Work with a style's draft, the unpublished copy edits are made against",
)];

/// The help line for a command group, by its whole path: `styles draft`.
pub fn command_group_about(path: &str) -> Option<&'static str> {
    COMMAND_GROUPS
        .iter()
        .find(|(group, _)| *group == path)
        .map(|(_, about)| *about)
}

/// The services the CLI builds commands from, out of the one-per-file
/// answers [`parse_spec`] gives.
///
/// This pass exists because an operation's service comes from
/// [`CLI_COMMAND_EXTENSION`], not from the file it was parsed out of — and
/// it might name a service that some *other* file is wired under, or one no
/// file is wired under at all. So we can't know which operations a service
/// has until every file has been parsed. That's why `parse_spec` answers
/// per file, and this function assembles the real surface afterward. Three
/// consequences:
///
///   - **A service whose operations all moved away is dropped**, instead
///     of built as an empty command group. `maps` and `vectortiles` are
///     exactly that today — their one operation each now targets
///     `tilesets`. An empty group would show up in `mapbox --help` and then
///     get refused by its own `subcommand_required(true)`.
///   - **A service with no file wired under it** takes its title and
///     description from [`MERGED_SERVICES`].
///   - **Order is first appearance**, across files in
///     [`effective_spec_entries`] order — so a service ends up wherever its
///     operations first show up.
pub fn regroup_by_service(parsed: Vec<ServiceSpec>) -> Vec<ServiceSpec> {
    // Grabbed before `parsed` is consumed: a service that still owns a
    // file keeps that file's `info`, which only exists in `parsed`.
    let from_specs: Vec<(String, String, Option<String>)> = parsed
        .iter()
        .map(|svc| (svc.name.clone(), svc.title.clone(), svc.description.clone()))
        .collect();

    let mut order: Vec<String> = vec![];
    let mut grouped: std::collections::HashMap<String, Vec<Operation>> =
        std::collections::HashMap::new();

    for svc in parsed {
        for op in svc.operations {
            let service = op.service.clone();
            if !grouped.contains_key(&service) {
                order.push(service.clone());
            }
            grouped.entry(service).or_default().push(op);
        }
    }

    order
        .into_iter()
        .map(|name| {
            let (title, description) = from_specs
                .iter()
                .find(|(spec_name, _, _)| *spec_name == name)
                .map(|(_, title, description)| (title.clone(), description.clone()))
                .or_else(|| {
                    MERGED_SERVICES
                        .iter()
                        .find(|(merged, _, _)| *merged == name)
                        .map(|(_, title, description)| {
                            (title.to_string(), Some(description.to_string()))
                        })
                })
                .unwrap_or_else(|| {
                    panic!(
                        "`{name}` is named by an operation's `{CLI_COMMAND_EXTENSION}` but no \
                         spec file is wired under it — give it a MERGED_SERVICES row saying \
                         what it is"
                    )
                });

            ServiceSpec {
                operations: grouped.remove(&name).unwrap_or_default(),
                name,
                title,
                description,
            }
        })
        .collect()
}

/// One spec file, as far as it can be read on its own.
///
/// The `name`, `title` and `description` belong to the file, but the
/// operations might not — each carries whatever service its own
/// [`CLI_COMMAND_EXTENSION`] names. [`regroup_by_service`] is what turns a
/// list of these into the services the CLI actually builds. Nothing else
/// should treat one of these as a real service.
pub fn parse_spec(service_name: &str, yaml: &str) -> Result<ServiceSpec> {
    let doc: Value = serde_yaml::from_str(yaml)
        .with_context(|| format!("Failed to parse YAML for service '{}'", service_name))?;

    let title = doc["info"]["title"]
        .as_str()
        .unwrap_or(service_name)
        .to_string();

    // Flattened to one line (unlike a parameter's description): this
    // renders as a service's `long_about`, where the spec's own line
    // wrapping doesn't help.
    let description = doc["info"]["description"]
        .as_str()
        .map(|text| first_paragraph(text).replace('\n', " "));

    let base_url = doc["servers"]
        .as_sequence()
        .and_then(|s| s.first())
        .and_then(|s| s["url"].as_str())
        .unwrap_or("https://api.mapbox.com")
        .trim_end_matches('/')
        .to_string();

    let paths = match doc["paths"].as_mapping() {
        Some(m) => m,
        None => {
            return Ok(ServiceSpec {
                name: service_name.to_string(),
                title,
                description,
                operations: vec![],
            })
        }
    };

    let mut operations = vec![];

    for (path_key, path_item) in paths {
        let path_str = path_key.as_str().unwrap_or("");

        let path_level_params = collect_parameters(path_item, "parameters", &doc);

        for method in &["get", "post", "put", "patch", "delete"] {
            let op = &path_item[method];
            if op.is_null() || !op.is_mapping() {
                continue;
            }

            let operation_id = op["operationId"].as_str();
            let summary = op["summary"].as_str().unwrap_or("").to_string();

            let generated_name = if let Some(id) = operation_id {
                camel_to_kebab(id)
            } else if !summary.is_empty() {
                str_to_kebab(&summary)
            } else {
                format!("{}-{}", method, str_to_kebab(path_str))
            };

            let (generated_command, aliases, hidden_aliases) = command_names(
                generated_name,
                operation_id.and_then(|id| alias_for(service_name, id)),
            );

            // The extension has the final say on both where the command
            // lives and what it's called. The generated name is only used
            // when a spec declares no extension. See `CLI_COMMAND_EXTENSION`.
            let (service, command_path) = cli_command_target(op, operation_id)?
                .unwrap_or_else(|| (service_name.to_string(), vec![generated_command]));

            let desc = op["description"].as_str().map(|s| s.to_string());

            let mut op_params = path_level_params.clone();
            op_params.extend(collect_parameters(op, "parameters", &doc));

            let mut seen = std::collections::HashSet::new();
            op_params.retain(|p| seen.insert(p.name.clone()));

            // Skip access_token params (handled globally)
            op_params.retain(|p| {
                !matches!(
                    p.name.to_lowercase().as_str(),
                    "access_token" | "accesstoken"
                )
            });

            let mut path_params = vec![];
            let mut query_params = vec![];

            for mut p in op_params {
                // Auto-filled from the global `--username`; see
                // ACCOUNT_PLACEHOLDERS.
                if ACCOUNT_PLACEHOLDERS.contains(&p.name.as_str()) {
                    continue;
                }
                if let Some(arg_name) = arg_name_override(service_name, &p.name) {
                    p.arg_name = arg_name.to_string();
                }
                if path_str.contains(&format!("{{{}}}", p.name)) {
                    path_params.push(p);
                } else {
                    query_params.push(p);
                }
            }

            path_params.sort_by_key(|p| {
                path_str
                    .find(&format!("{{{}}}", p.name))
                    .unwrap_or(usize::MAX)
            });

            let body = parse_request_body(op, &doc).map(|body| {
                match operation_id.and_then(|id| content_type_override(service_name, id)) {
                    Some(content_type) => RequestBody {
                        content_types: vec![content_type.to_string()],
                        ..body
                    },
                    None => body,
                }
            });

            let disabled_scope =
                operation_id.and_then(|id| unsupported_scope_for(service_name, id));

            operations.push(Operation {
                service,
                command_path,
                summary: if summary.is_empty() {
                    format!("{} {}", method.to_uppercase(), path_str)
                } else {
                    summary
                },
                description: desc,
                method: method.to_uppercase(),
                path_template: path_str.to_string(),
                path_params,
                query_params,
                body,
                base_url: base_url.clone(),
                disabled_scope,
                detail: None,
                listing: None,
                deprecated: op["deprecated"].as_bool().unwrap_or(false),
                withheld: operation_id.is_some_and(|id| withheld(service_name, id)),
                aliases,
                hidden_aliases,
            });
        }
    }

    link_detail_operations(&mut operations);

    Ok(ServiceSpec {
        name: service_name.to_string(),
        title,
        description,
        operations,
    })
}

/// Reads an operation's `requestBody` into the media types it declares.
///
/// `Some` with an empty `content_types` is deliberate, not a bug: a
/// `requestBody` with no `content` still means the operation takes a body
/// — the same thing the old `has_body` bool recorded — and
/// [`RequestBody::accepts_json`] still treats it as JSON.
fn parse_request_body(op: &Value, full_spec: &Value) -> Option<RequestBody> {
    let request_body = &op["requestBody"];
    // A shared body lives under `components/requestBodies` and arrives here
    // as a `$ref`, which is still a mapping — so without resolving it, the
    // check below would treat an operation that requires a body as one
    // with no declared type and no `required` flag. No bundled spec shares
    // a body today, but a future sync could add one.
    let request_body = request_body["$ref"]
        .as_str()
        .and_then(|reference| resolve_ref(full_spec, reference))
        .unwrap_or(request_body);

    if !request_body.is_mapping() {
        return None;
    }

    let required = request_body["required"].as_bool().unwrap_or(false);

    let content = match request_body["content"].as_mapping() {
        Some(content) => content,
        None => {
            return Some(RequestBody {
                required,
                content_types: vec![],
                multipart_field: None,
            })
        }
    };

    let content_types: Vec<String> = content
        .keys()
        .filter_map(|key| key.as_str())
        .map(|key| key.to_string())
        .collect();

    let multipart_field = content
        .iter()
        .find(|(key, _)| key.as_str().map(essence).as_deref() == Some(MULTIPART))
        .and_then(|(_, media_type)| multipart_file_field(&media_type["schema"], full_spec));

    Some(RequestBody {
        required,
        content_types,
        multipart_field,
    })
}

/// The multipart property that carries the uploaded files.
///
/// We prefer a property whose schema is `format: binary` (directly, or as
/// an array's items) over just taking the first property. A multipart body
/// can mix files with ordinary text fields, and posting the bytes under the
/// wrong field name gives a 400 that doesn't name either one.
fn multipart_file_field(schema: &Value, full_spec: &Value) -> Option<String> {
    let schema = match schema["$ref"].as_str() {
        Some(reference) => resolve_ref(full_spec, reference)?,
        None => schema,
    };

    let properties = schema["properties"].as_mapping()?;
    let named = |(key, _): &(&Value, &Value)| key.as_str().map(|s| s.to_string());

    properties
        .iter()
        .find(|(_, value)| is_binary_schema(value))
        .as_ref()
        .and_then(named)
        .or_else(|| properties.iter().next().as_ref().and_then(named))
}

/// Whether a schema describes bytes, either on its own or as an array of them.
fn is_binary_schema(schema: &Value) -> bool {
    let binary = |node: &Value| node["format"].as_str() == Some("binary");
    binary(schema) || binary(&schema["items"])
}

fn collect_parameters(node: &Value, key: &str, full_spec: &Value) -> Vec<Parameter> {
    let params_val = &node[key];
    let seq = match params_val.as_sequence() {
        Some(s) => s,
        None => return vec![],
    };

    let mut result = vec![];
    for item in seq {
        let resolved = if let Some(ref_str) = item["$ref"].as_str() {
            match resolve_ref(full_spec, ref_str) {
                Some(v) => v.clone(),
                None => continue,
            }
        } else {
            item.clone()
        };

        if let Some(param) = parse_parameter(&resolved) {
            result.push(param);
        }
    }
    result
}

/// Links each listing to the operation that shows one of its items, and
/// each of those back to its listing.
///
/// A detail operation is a `GET` whose path is the listing's path plus one
/// more parameter — e.g. `/styles/v1/{username}` and
/// `/styles/v1/{username}/{style_id}`. That extra parameter has to
/// identify an item, so account-scoping parameters don't count:
/// `/tokens/v2` extended by `{username}` is still just another listing,
/// not a way to look at one token. Treating it as one would wrongly offer
/// `mapbox accounts list-tokens <username>` as "see one of these".
fn link_detail_operations(operations: &mut [Operation]) {
    // Only real commands can be pointed at here. This hint reaches a
    // caller as something to run — `--schema` publishes it as
    // `detail_command` — and many withheld or unsupported operations are
    // GETs on exactly these kinds of paths. Without this filter, we could
    // point someone at a command that doesn't actually work.
    let candidates: Vec<(String, String)> = operations
        .iter()
        .filter(|op| op.method == "GET" && op.is_exposed())
        .map(|op| (op.path_template.clone(), op.command()))
        .collect();

    for op in operations.iter_mut().filter(|op| op.method == "GET") {
        let prefix = format!("{}/{{", op.path_template);
        op.detail = candidates.iter().find_map(|(path, command)| {
            // The name comes from the path, not the parameter list — some
            // specs use a placeholder in the path without ever declaring a
            // matching parameter for it.
            let name = path
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix('}'))?;
            if name.is_empty() || ACCOUNT_PLACEHOLDERS.contains(&name) {
                return None;
            }
            Some(DetailOperation {
                command: command.clone(),
                parameter: name.replace('_', "-"),
            })
        });
    }

    // The same pairing, read backwards, for when it matters most: a 404
    // from a detail operation usually means the id doesn't exist, and the
    // listing is where valid ids come from. Only a listing that's itself a
    // real command gets named here — the pass above pairs on paths alone,
    // so an unexposed GET could otherwise hold a `detail` link while not
    // being runnable itself.
    let listings: Vec<(String, ListingOperation)> = operations
        .iter()
        .filter(|op| op.is_exposed())
        .filter_map(|op| {
            let detail = op.detail.as_ref()?;
            Some((
                detail.command.clone(),
                ListingOperation {
                    command: op.command(),
                    path_template: op.path_template.clone(),
                },
            ))
        })
        .collect();

    for op in operations.iter_mut() {
        // Matched on the whole command, service included, not just the
        // last word — `styles get` and `styles draft get` both end in
        // `get`.
        let command = op.command();
        op.listing = listings
            .iter()
            .find(|(detail, _)| *detail == command)
            .map(|(_, listing)| listing.clone());
    }
}

/// Everything up to the first blank line, with line breaks kept as the
/// spec wrote them.
///
/// Spec prose usually puts the actual definition first, then reference
/// material after a blank line — an options table, per-country notes,
/// worked examples. The first paragraph is the part that describes the
/// thing itself.
///
/// We keep the line breaks because `--help` has always rendered them, and
/// dropping them would needlessly reflow a few arguments' help text — plus
/// they're the only structure that bulleted descriptions have.
fn first_paragraph(text: &str) -> String {
    text.lines()
        .take_while(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// A YAML scalar as the text that would go into a URL. `None` for anything
/// with no single obvious spelling — a mapping, a sequence, a null.
fn scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn parse_parameter(val: &Value) -> Option<Parameter> {
    let name = val["name"].as_str()?.to_string();
    let required = val["required"].as_bool().unwrap_or(false);
    // The first paragraph, same rule as the service description above.
    //
    // This used to cut at the first `.`, which works fine for a line of
    // help but breaks `--schema`, where callers actually reason from this
    // field. That cut landed inside `username.tileset-id`, inside `(range
    // -85.0511, 85.0511)`, and inside `e.g.` — publishing a truncated
    // identifier and a wrong bound. Keeping the whole text was the other
    // extreme: `geocoder --types` alone is 4.6 KB of feature-type
    // reference, and the schema gets read on every call. A paragraph
    // avoids both problems, and leaves the appendices out.
    // `crate::first_sentence` shortens it further for `--help`, which
    // still renders exactly as it always has.
    let description = val["description"].as_str().map(first_paragraph);

    let schema = &val["schema"];
    let type_str = schema["type"].as_str().unwrap_or("string");
    let is_boolean = type_str == "boolean";
    let numeric = match type_str {
        "integer" => Some(Numeric::Integer),
        "number" => Some(Numeric::Float),
        _ => None,
    };

    // Numbers and booleans count too. Keeping only strings used to
    // silently drop `tilesize: enum [256, 512]` — the one non-string enum
    // among the bundled specs' parameters — so the CLI would accept `300`,
    // send it, and let the API reject it instead. Everything downstream
    // wants the value as text anyway, since it's going into a URL.
    let enum_values: Vec<String> = schema["enum"]
        .as_sequence()
        .map(|values| values.iter().filter_map(scalar_to_string).collect())
        .unwrap_or_default();

    let arg_name = name.replace('_', "-");

    Some(Parameter {
        name,
        arg_name,
        required,
        description,
        enum_values,
        is_boolean,
        numeric,
    })
}

pub fn resolve_ref<'a>(spec: &'a Value, ref_str: &str) -> Option<&'a Value> {
    if !ref_str.starts_with('#') {
        return None;
    }
    let path = ref_str.trim_start_matches('#').trim_start_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    let mut current = spec;
    for part in &parts {
        current = current.get(part)?;
    }
    Some(current)
}

pub fn camel_to_kebab(s: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                let prev_lower = chars[i - 1].is_lowercase();
                let next_lower = chars.get(i + 1).map(|c| c.is_lowercase()).unwrap_or(false);
                if prev_lower || (i > 1 && next_lower && chars[i - 1].is_uppercase()) {
                    result.push('-');
                }
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

pub fn str_to_kebab(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(yaml: &str) -> ServiceSpec {
        parse_spec("svc", yaml).expect("fixture parses")
    }

    const PAIRED: &str = r#"
openapi: 3.0.0
info: { title: T }
paths:
  /styles/v1/{username}:
    get: { operationId: listStyles, summary: List }
  /styles/v1/{username}/{style_id}:
    get: { operationId: getStyle, summary: Get }
"#;

    const REQUIRED_BODIES: &str = r#"
openapi: 3.0.0
info: { title: T }
paths:
  /a:
    post:
      operationId: mustHaveBody
      requestBody:
        required: true
        content: { application/json: { schema: { type: object } } }
  /b:
    post:
      operationId: mayHaveBody
      requestBody:
        required: false
        content: { application/json: { schema: { type: object } } }
  /c:
    post:
      operationId: silentAboutBody
      requestBody:
        content: { application/json: { schema: { type: object } } }
  /d:
    post:
      operationId: sharedBody
      requestBody: { $ref: '#/components/requestBodies/Shared' }
components:
  requestBodies:
    Shared:
      required: true
      content: { application/json: { schema: { type: object } } }
"#;

    /// The spec's own answer, not a default.
    ///
    /// `--schema` publishes this as an argument's `required`, and the tests
    /// on the schema side compare it against the same parsed value — so this
    /// is the only place that can catch the field being read wrong, or not
    /// read at all.
    #[test]
    fn a_body_is_required_when_the_spec_says_so() {
        let svc = service(REQUIRED_BODIES);
        let required_of = |name: &str| {
            svc.operations
                .iter()
                .find(|op| op.command_name() == name)
                .unwrap_or_else(|| panic!("{name} parsed"))
                .body
                .as_ref()
                .unwrap_or_else(|| panic!("{name} has a body"))
                .required
        };

        assert!(required_of("must-have-body"));
        assert!(!required_of("may-have-body"));
        // Absent means optional, which is what OpenAPI says it means.
        assert!(!required_of("silent-about-body"));
        // A body factored out into `components/requestBodies` keeps its
        // answer; read through the `$ref` it would silently become optional.
        assert!(required_of("shared-body"));
    }

    #[test]
    fn an_enum_of_numbers_is_still_an_enum() {
        let svc = service(
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /tiles/{tilesize}:
    get:
      operationId: sized
      parameters:
        - { name: tilesize, in: path, required: false, schema: { type: integer, enum: [256, 512] } }
"#,
        );

        let param = &svc.operations[0].path_params[0];
        assert_eq!(param.enum_values, ["256", "512"]);
    }

    /// The paragraph, not the sentence — the cut used to land inside
    /// `username.tileset-id` — and not the appendix either.
    #[test]
    fn a_description_keeps_its_first_paragraph_whole() {
        let svc = service(
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /t/{id}:
    get:
      operationId: get
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
          description: |
            Tileset ID in the format `username.id` (e.g. `mapbox.satellite`).
            Order matters.

            Everything after the break is reference material.
"#,
        );

        assert_eq!(
            svc.operations[0].path_params[0].description.as_deref(),
            Some(
                "Tileset ID in the format `username.id` (e.g. `mapbox.satellite`).\nOrder matters."
            )
        );
    }

    #[test]
    fn a_listing_is_linked_to_the_operation_that_shows_one_item() {
        let svc = service(PAIRED);
        let list = svc
            .operations
            .iter()
            .find(|o| o.command_name() == "list-styles")
            .expect("listing built");
        let detail = list.detail.as_ref().expect("linked");

        assert_eq!(detail.command, "svc get-style");
        assert_eq!(detail.parameter, "style-id");
    }

    /// And the same link read backwards, which is what a 404 needs: the
    /// operation that shows one item knows the listing its ids came from.
    #[test]
    fn the_operation_that_shows_one_item_knows_its_listing() {
        let svc = service(PAIRED);

        let listing = operation(&svc, "get-style")
            .listing
            .as_ref()
            .expect("linked back");
        assert_eq!(listing.command, "svc list-styles");
        assert_eq!(
            listing.path_template, "/styles/v1/{username}",
            "the listing's own path is what says which parameters it needs too"
        );
        assert!(
            operation(&svc, "list-styles").listing.is_none(),
            "a listing has no listing of its own to be sent back to"
        );
    }

    /// A listing that isn't itself a command must never be named as one.
    /// This fixture gives a listing-shaped operation an operationId from
    /// `UNSUPPORTED_OPERATIONS` (`getFontCoverage`, needing a scope no
    /// login can carry), so it's filtered out of the surface. Pointing a
    /// 404 at it would otherwise tell the caller to run something that
    /// doesn't exist. Which entry we borrow doesn't matter, as long as it
    /// still needs a scope DCR won't grant — `listFonts` served this
    /// purpose until `fonts:list` became registrable.
    #[test]
    fn an_unusable_listing_is_never_named() {
        let svc = parse_spec(
            "fonts",
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /fonts/v1/{owner}:
    get: { operationId: getFontCoverage, summary: List }
  /fonts/v1/{owner}/{fileName}:
    get: { operationId: getFontFile, summary: One }
"#,
        )
        .expect("fixture parses");

        assert!(
            operation(&svc, "get-font-coverage")
                .disabled_scope
                .is_some(),
            "the fixture stops being about anything if this becomes usable"
        );
        assert!(
            operation(&svc, "get-font-file").is_exposed(),
            "the detail side has to be a command, or the assertion below              would hold for the wrong reason"
        );
        assert!(operation(&svc, "get-font-file").listing.is_none());
    }

    /// `/tokens/v2` extended by `{username}` is another listing, not a way to
    /// look at one token. Suggesting it would promise something the API
    /// cannot do.
    #[test]
    fn an_account_scoped_path_is_not_a_detail_operation() {
        let svc = service(
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /tokens/v2:
    get: { operationId: retrieveToken, summary: Retrieve }
  /tokens/v2/{username}:
    get: { operationId: listTokens, summary: List }
"#,
        );

        for op in &svc.operations {
            assert!(op.detail.is_none(), "{} was linked", op.command_name());
        }
    }

    #[test]
    fn an_operation_with_no_such_sibling_is_left_unlinked() {
        let svc = service(
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /scopes/v1/{username}:
    get: { operationId: listScopes, summary: List }
"#,
        );
        assert!(svc.operations[0].detail.is_none());
    }

    fn operation<'a>(svc: &'a ServiceSpec, name: &str) -> &'a Operation {
        svc.operations
            .iter()
            .find(|o| o.command_name() == name)
            .expect("operation built")
    }

    /// Every content type the bundled specs actually declare, in one fixture,
    /// so the mapping from spec to flag is checked rather than assumed.
    const BODIES: &str = r#"
openapi: 3.0.0
info: { title: T }
paths:
  /json:
    post:
      operationId: createStyle
      requestBody:
        content:
          application/json:
            schema: { type: object }
  /raw:
    put:
      operationId: uploadSpriteImage
      requestBody:
        content:
          image/svg+xml:
            schema: { type: string, format: binary }
  /form:
    post:
      operationId: batchUploadSprite
      requestBody:
        content:
          multipart/form-data:
            schema:
              type: object
              required: [images]
              properties:
                images:
                  type: array
                  items: { type: string, format: binary }
  /either:
    post:
      operationId: initUpload
      requestBody:
        content:
          application/octet-stream:
            schema: { type: string, format: binary }
          application/json:
            schema: { type: object }
  /none:
    get:
      operationId: listStyles
"#;

    #[test]
    fn a_json_body_asks_for_data_and_never_for_a_file() {
        let svc = service(BODIES);
        let body = operation(&svc, "create-style").body.as_ref().expect("body");
        assert!(body.accepts_json());
        assert_eq!(body.file_content_type(), None);
        assert!(!body.is_multipart());
    }

    /// The declared type is what the executor puts on the wire, so it has to
    /// survive parsing exactly — not be normalized into a guess.
    #[test]
    fn a_raw_body_keeps_the_media_type_the_spec_wrote() {
        let svc = service(BODIES);
        let body = operation(&svc, "upload-sprite-image")
            .body
            .as_ref()
            .expect("body");
        assert_eq!(body.file_content_type(), Some("image/svg+xml"));
        assert!(!body.accepts_json());
        assert!(!body.is_multipart());
    }

    #[test]
    fn a_multipart_body_reports_the_field_its_schema_names() {
        let svc = service(BODIES);
        let body = operation(&svc, "batch-upload-sprite")
            .body
            .as_ref()
            .expect("body");
        assert!(body.is_multipart());
        assert_eq!(body.file_content_type(), Some("multipart/form-data"));
        assert_eq!(body.multipart_field.as_deref(), Some("images"));
        assert!(!body.accepts_json());
    }

    /// `initUpload` declares both. Neither flag may shadow the other.
    #[test]
    fn a_body_declaring_bytes_and_json_offers_both() {
        let svc = service(BODIES);
        let body = operation(&svc, "init-upload").body.as_ref().expect("body");
        assert!(body.accepts_json());
        assert_eq!(body.file_content_type(), Some("application/octet-stream"));
    }

    #[test]
    fn an_operation_with_no_request_body_has_none() {
        let svc = service(BODIES);
        assert!(operation(&svc, "list-styles").body.is_none());
    }

    /// A multipart form may carry ordinary text fields beside its files.
    /// Uploading the bytes under the wrong name is a 400 that names neither,
    /// so the binary property wins over document order.
    #[test]
    fn the_file_field_is_the_binary_one_not_the_first_one() {
        let svc = service(
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /form:
    post:
      operationId: uploadThings
      requestBody:
        content:
          multipart/form-data:
            schema:
              type: object
              properties:
                description: { type: string }
                attachments:
                  type: array
                  items: { type: string, format: binary }
"#,
        );
        let body = operation(&svc, "upload-things")
            .body
            .as_ref()
            .expect("body");
        assert_eq!(body.multipart_field.as_deref(), Some("attachments"));
    }

    /// A `requestBody` that declares no content still means the operation
    /// takes one. That is what the old `has_body` bool recorded, and `--data`
    /// has to keep working for it.
    #[test]
    fn a_body_with_no_declared_content_is_still_a_json_body() {
        let svc = service(
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /thing:
    post:
      operationId: doThing
      requestBody:
        required: true
"#,
        );
        let body = operation(&svc, "do-thing").body.as_ref().expect("body");
        assert!(body.accepts_json());
        assert_eq!(body.file_content_type(), None);
    }

    /// `deprecated: true` is the one thing a spec can say about an
    /// operation's future, and until it was read here the CLI ran such an
    /// operation with nothing to say about it.
    #[test]
    fn a_deprecated_operation_says_so_and_a_silent_one_does_not() {
        let svc = service(
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /old:
    get:
      operationId: getOld
      summary: Old
      deprecated: true
  /new:
    get: { operationId: getNew, summary: New }
"#,
        );
        assert!(operation(&svc, "get-old").deprecated);
        assert!(!operation(&svc, "get-new").deprecated);
    }

    /// Two specs that could sit under one service name, told apart by the
    /// operation each declares — so a merge test can name which side won
    /// rather than compare opaque strings.
    const FROM_MAPBOX: &str = r#"
openapi: 3.0.0
info: { title: Upstream }
paths:
  /a:
    get: { operationId: fromMapbox, summary: Upstream }
"#;

    const FROM_CUSTOM: &str = r#"
openapi: 3.0.0
info: { title: Hand-written }
paths:
  /a:
    get: { operationId: fromCustom, summary: Hand-written }
"#;

    fn entry(name: &'static str, yaml: &'static str) -> SpecEntry {
        SpecEntry { name, yaml }
    }

    fn names(entries: &[SpecEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.name).collect()
    }

    /// The command a merged entry would actually generate — the only
    /// visible sign of "which spec won".
    ///
    /// Named differently from the real `command_names` (which computes one
    /// operation's name and aliases) so `use super::*` doesn't let this
    /// test-only helper shadow it.
    fn merged_command_names(entries: &[SpecEntry], name: &str) -> Vec<String> {
        let entry = entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("`{name}` is in the merged list"));
        service(entry.yaml)
            .operations
            .iter()
            .map(|op| op.command_name().to_string())
            .collect()
    }

    /// What the custom directory is for: the hand-written spec *replaces* the
    /// upstream one for that service, rather than adding a second command
    /// tree under the same name.
    #[test]
    fn a_custom_spec_wins_over_the_mapbox_spec_of_the_same_name() {
        let merged = merge_entries(
            &[entry("search", FROM_MAPBOX)],
            &[entry("search", FROM_CUSTOM)],
        );

        assert_eq!(names(&merged), ["search"]);
        assert_eq!(merged[0].yaml, FROM_CUSTOM);
        assert_eq!(merged_command_names(&merged, "search"), ["from-custom"]);
    }

    /// A service with no override passes through untouched — the path every
    /// `MAPBOX_SPEC_ENTRIES` service still takes except the ones
    /// `CUSTOM_SPEC_ENTRIES` names (`search`, as of this PR).
    #[test]
    fn a_mapbox_service_with_no_override_passes_through_untouched() {
        let mapbox = [entry("styles", FROM_MAPBOX), entry("fonts", FROM_MAPBOX)];
        let merged = merge_entries(&mapbox, &[entry("styles", FROM_CUSTOM)]);

        // Declaration order survives the swap: the overridden entry stays
        // where it was rather than moving to the end.
        assert_eq!(names(&merged), ["styles", "fonts"]);
        assert_eq!(merged[1].yaml, FROM_MAPBOX);
        assert_eq!(merged_command_names(&merged, "fonts"), ["from-mapbox"]);
    }

    /// The other half of the point: a service openapi-specs has no spec for
    /// at all still reaches the surface.
    #[test]
    fn a_custom_only_service_reaches_the_merged_list() {
        let merged = merge_entries(
            &[entry("styles", FROM_MAPBOX)],
            &[entry("search", FROM_CUSTOM)],
        );

        assert_eq!(names(&merged), ["styles", "search"]);
        assert_eq!(merged_command_names(&merged, "search"), ["from-custom"]);
    }

    /// Disjoint tables merge to their union — nothing dropped for being on
    /// the wrong side, and nothing counted twice.
    #[test]
    fn two_disjoint_tables_merge_without_losing_or_duplicating_a_service() {
        let merged = merge_entries(
            &[entry("styles", FROM_MAPBOX), entry("fonts", FROM_MAPBOX)],
            &[entry("search", FROM_CUSTOM), entry("places", FROM_CUSTOM)],
        );

        assert_eq!(names(&merged), ["styles", "fonts", "search", "places"]);
    }

    /// The shipped tables merge into one surface: no name claimed twice, and
    /// every wired upstream service still present.
    #[test]
    fn the_shipped_tables_merge_into_one_surface() {
        let effective = effective_spec_entries();

        let mut seen = names(&effective);
        let declared = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            declared,
            "a service name reaches the surface twice"
        );

        for wired in MAPBOX_SPEC_ENTRIES {
            assert!(
                effective.iter().any(|entry| entry.name == wired.name),
                "`{}` is wired from openapi-specs but missing from the effective list",
                wired.name
            );
        }

        // `build_app` registers six hand-written top-level commands
        // alongside the generated ones (`auth` has no `COMMAND` const to
        // borrow; the rest do). A spec table claiming any of these names
        // would collide the same way two same-named spec entries would,
        // and `merge_entries` has no way to catch a clash outside its own
        // tables. We pull the name from each module's `COMMAND` const
        // instead of hard-coding the string again, so a seventh
        // hand-written command is covered automatically once it lands.
        for reserved in [
            "auth",
            crate::generate_skills::COMMAND,
            crate::completion::COMMAND,
            crate::uninstall::COMMAND,
            crate::account_usage::COMMAND,
            crate::tilesets_cli::COMMAND,
        ] {
            assert!(
                !effective.iter().any(|entry| entry.name == reserved),
                "`{reserved}` is a hand-written command; a spec entry of the same name would collide with it"
            );
        }
    }

    /// Every [`COMMAND_ALIASES`] row reaches an operation that exists.
    ///
    /// `alias_for` returns `None` for a row with a misspelled `service` or
    /// `operationId` — the exact same result as for an operation no row
    /// mentions at all. So the alias would silently never appear, the
    /// command would keep its generated name, and nothing would notice.
    /// Worse, `tests/api_command_surface.rs` would then pin that broken row
    /// as if it worked. This test exists to catch that: does each row
    /// actually do something?
    ///
    /// Runs against the effective (merged) specs, not a fixture, because
    /// what actually breaks is a row pointing at an `operationId` those
    /// specs don't have — either a typo, or a rename upstream that a later
    /// sync picked up.
    #[test]
    fn every_command_alias_names_a_real_operation() {
        let effective = effective_spec_entries();
        for (service_name, operation_id, alias, show_generated_name) in COMMAND_ALIASES {
            let entry = effective
                .iter()
                .find(|entry| entry.name == *service_name)
                .unwrap_or_else(|| {
                    panic!("COMMAND_ALIASES names `{service_name}`, which is not a service")
                });
            let svc = parse_spec(entry.name, entry.yaml).expect("bundled spec parses");

            // The alias shows up either as the command's real name or as
            // one it also answers to, depending on the row. Either way,
            // exactly one operation should carry it — zero if the row is
            // dead.
            let carriers: Vec<&str> = svc
                .operations
                .iter()
                .filter(|op| {
                    if *show_generated_name {
                        op.aliases.contains(alias)
                    } else {
                        op.command_name() == *alias
                    }
                })
                .map(|op| op.command_name())
                .collect();

            assert_eq!(
                carriers.len(),
                1,
                "`{service_name}` has no operation `{operation_id}` for the alias \
                 `{alias}` to rename — check the spelling against the spec, or drop \
                 the row if the spec renamed it"
            );
        }
    }

    /// Merging against a truly empty custom table (not `CUSTOM_SPEC_ENTRIES`,
    /// which has `search` in it) should change nothing at all. The test
    /// above, `a_mapbox_service_with_no_override_passes_through_untouched`,
    /// covers the general case with a real override; this is the edge case
    /// with none, pinned against the real shipped table.
    #[test]
    fn merging_an_empty_custom_table_changes_nothing() {
        let merged = merge_entries(MAPBOX_SPEC_ENTRIES, &[]);
        assert_eq!(names(&merged), names(MAPBOX_SPEC_ENTRIES));
    }

    /// One duplicate shape `merge_entries` used to swallow silently: two
    /// `CUSTOM_SPEC_ENTRIES` entries with the same name that also exists in
    /// `MAPBOX_SPEC_ENTRIES`. Both the first-match `find` and the
    /// unmatched-name filter treat this as an ordinary override, so the
    /// output looks fine — no repeated name, every mapbox name present —
    /// and the merged-list assertions above can't catch it. Only a
    /// table-level check like this one can.
    #[test]
    #[should_panic(expected = "CUSTOM_SPEC_ENTRIES lists `search` more than once")]
    fn a_repeated_custom_name_panics_instead_of_silently_dropping_one() {
        merge_entries(
            &[entry("search", FROM_MAPBOX)],
            &[entry("search", FROM_CUSTOM), entry("search", FROM_MAPBOX)],
        );
    }

    /// Both values of `show_generated_name`. No row in the real table
    /// exercises `true` — they're all `false` today — so this test covers
    /// it directly.
    #[test]
    fn a_visible_alias_keeps_the_generated_name_and_a_hidden_one_replaces_it() {
        let generated = || "get-v4tilesets-tilequery-lon-lat-json".to_string();

        let (name, aliases, hidden) = command_names(generated(), Some(("get-tilequery", true)));
        assert_eq!(name, generated());
        assert_eq!(aliases, ["get-tilequery"]);
        assert!(hidden.is_empty());

        let (name, aliases, hidden) = command_names(generated(), Some(("get-tilequery", false)));
        assert_eq!(name, "get-tilequery");
        assert!(aliases.is_empty());
        assert_eq!(hidden, [generated()]);

        let (name, aliases, hidden) = command_names(generated(), None);
        assert_eq!(name, generated());
        assert!(aliases.is_empty());
        assert!(hidden.is_empty());
    }

    /// `directions.yaml`'s `profile` path parameter is a real collision with
    /// the global `--profile` (credentials profile) argument's id — clap has
    /// one namespace of ids per command, and the generated positional would
    /// otherwise silently replace the global one. This is the regression
    /// test for `mapbox directions route mapbox/driving …` failing with
    /// `Invalid profile name "mapbox/driving"` before [`ARG_NAME_OVERRIDES`]
    /// existed: the parsed parameter's `arg_name` must differ from the
    /// global's id, while `name` stays `profile` so the path template's
    /// `{profile}` placeholder still resolves.
    #[test]
    fn the_directions_profile_parameter_does_not_collide_with_the_global_flag() {
        let spec = parse_spec(
            "directions",
            include_str!("../custom-openapi/directions/openapi/directions.yaml"),
        )
        .expect("directions.yaml parses");

        let route = spec
            .operations
            .iter()
            .find(|op| op.command_path == ["route"])
            .expect("the route operation exists");

        let profile = route
            .path_params
            .iter()
            .find(|p| p.name == "profile")
            .expect("a path parameter named profile");

        assert_ne!(
            profile.arg_name, "profile",
            "must not collide with the global --profile id"
        );
        assert_eq!(
            profile.enum_values,
            [
                "mapbox/driving-traffic",
                "mapbox/driving",
                "mapbox/walking",
                "mapbox/cycling"
            ]
        );
    }

    /// Same regression as `the_directions_profile_parameter_does_not_collide…`
    /// above, for the second spec that ran into it — `ARG_NAME_OVERRIDES`
    /// taking effect is per-row, so a second entry earns its own proof
    /// rather than trusting the first test to cover it.
    #[test]
    fn the_isochrone_profile_parameter_does_not_collide_with_the_global_flag() {
        let spec = parse_spec(
            "isochrone",
            include_str!("../custom-openapi/isochrone/openapi/isochrone.yaml"),
        )
        .expect("isochrone.yaml parses");

        let contours = spec
            .operations
            .iter()
            .find(|op| op.command_path == ["contours"])
            .expect("the contours operation exists");

        let profile = contours
            .path_params
            .iter()
            .find(|p| p.name == "profile")
            .expect("a path parameter named profile");

        assert_ne!(
            profile.arg_name, "profile",
            "must not collide with the global --profile id"
        );
    }

    /// Same regression, for the third spec that ran into it.
    #[test]
    fn the_map_matching_profile_parameter_does_not_collide_with_the_global_flag() {
        let spec = parse_spec(
            "map-matching",
            include_str!("../custom-openapi/map-matching/openapi/map-matching.yaml"),
        )
        .expect("map-matching.yaml parses");

        let matched = spec
            .operations
            .iter()
            .find(|op| op.command_path == ["match"])
            .expect("the match operation exists");

        let profile = matched
            .path_params
            .iter()
            .find(|p| p.name == "profile")
            .expect("a path parameter named profile");

        assert_ne!(
            profile.arg_name, "profile",
            "must not collide with the global --profile id"
        );
    }

    /// Same regression, for the fourth spec that ran into it.
    #[test]
    fn the_matrix_profile_parameter_does_not_collide_with_the_global_flag() {
        let spec = parse_spec(
            "matrix",
            include_str!("../custom-openapi/matrix/openapi/matrix.yaml"),
        )
        .expect("matrix.yaml parses");

        let compute = spec
            .operations
            .iter()
            .find(|op| op.command_path == ["compute"])
            .expect("the compute operation exists");

        let profile = compute
            .path_params
            .iter()
            .find(|p| p.name == "profile")
            .expect("a path parameter named profile");

        assert_ne!(
            profile.arg_name, "profile",
            "must not collide with the global --profile id"
        );
    }

    #[test]
    fn arg_name_override_only_fires_for_the_row_it_names() {
        assert_eq!(
            arg_name_override("directions", "profile"),
            Some("routing-profile")
        );
        assert_eq!(
            arg_name_override("isochrone", "profile"),
            Some("routing-profile")
        );
        assert_eq!(
            arg_name_override("map-matching", "profile"),
            Some("routing-profile")
        );
        assert_eq!(
            arg_name_override("matrix", "profile"),
            Some("routing-profile")
        );
        assert_eq!(arg_name_override("directions", "coordinates"), None);
        assert_eq!(arg_name_override("styles", "profile"), None);
    }
}
