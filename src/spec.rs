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
    /// Set when this operation's spec-documented required scope does not exist
    /// as a registrable OAuth scope (see UNSUPPORTED_OPERATIONS below). A token
    /// obtained via `mapbox auth login` can never carry it, so the command refuses to
    /// run instead of failing with a confusing 403 at request time.
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
    /// Extra, visible names this command answers to and lists in `--help`
    /// and `--schema` — see [`COMMAND_ALIASES`]. Empty for every operation
    /// but the one whose spec-generated name is worth keeping alongside a
    /// better one.
    pub aliases: Vec<&'static str>,
    /// Extra names this command answers to but does not offer anyone — also
    /// from [`COMMAND_ALIASES`]. A caller already using one keeps working,
    /// and nothing publishes it: not `--help`, not `--schema`, not the docs
    /// page. Only `clap` and this field know it is there, which is why
    /// `api_command_surface`'s fixture reads the built command tree rather
    /// than the schema — the schema cannot see one by design.
    ///
    /// `String`, not `&'static str`: what lands here is the *generated*
    /// name (`camel_to_kebab` of the spec's own `operationId`), computed at
    /// parse time rather than written in the table.
    pub hidden_aliases: Vec<String>,
}

/// (service, operationId, the media type the API actually requires) for
/// bodies whose spec is wrong.
///
/// Not a workaround for a hard case — a correction. `starFile` declares
/// `application/json` with a `boolean` schema, and the service answers
/// `400 Must be plaintext true or false` to exactly that. Sending the same
/// `true` as `text/plain` returns 204. Verified with curl against
/// production: `application/json` and no content type both fail, `text/plain`
/// succeeds.
///
/// Kept as data rather than a branch in the executor so the divergence is
/// visible next to the operation it belongs to, and so a spec fix is a
/// deletion from this list.
const BODY_CONTENT_TYPE_OVERRIDES: &[(&str, &str, &str)] = &[("styles", "starFile", "text/plain")];

/// The media types an operation's request body may be sent as.
///
/// This used to be a bare `has_body: bool`, which threw the declared type
/// away and left the executor with one choice: call everything
/// `application/json`. That is right for the twenty JSON operations and
/// wrong for the three that are not — `uploadSpriteImage` wants raw SVG
/// bytes, `batchUploadSprite` a multipart form, `uploadChunk` raw bytes —
/// so those had no way to send a body at all.
#[derive(Debug, Clone)]
pub struct RequestBody {
    /// Whether the operation refuses to work without one.
    ///
    /// Read but not enforced: clap still accepts a bodyless invocation of
    /// `create-style`, and turning that into a local error is a change to
    /// what the CLI rejects rather than to what it describes. It is here so
    /// `--schema` can say which bodies the API insists on — thirteen of the
    /// sixteen body-carrying operations the CLI exposes, which between them
    /// declare eighteen flags — instead of calling every one of them
    /// optional.
    pub required: bool,
    /// Declared media types, in the order the spec lists them. Usually one;
    /// `initUpload` declares both `application/octet-stream` and
    /// `application/json`.
    pub content_types: Vec<String>,
    /// The multipart property the files go under — `images` for
    /// `batchUploadSprite`. Read from the schema rather than hard-coded:
    /// it is the field name the API matches on, so guessing it wrong fails
    /// at request time with nothing to point at.
    pub multipart_field: Option<String>,
}

pub const MULTIPART: &str = "multipart/form-data";

/// The path placeholders the global `--username` fills, rather than a
/// parameter of their own.
///
/// One list, because three places act on it and they have to agree:
/// `parse_spec` drops the matching parameters, `executor::execute`
/// substitutes them, `link_detail_operations` refuses to treat them as an
/// identifier, and `crate::schema` describes the flag that fills them. A
/// fourth spelling appearing in a spec and being added to only some of those
/// is the failure this prevents; `every_url_placeholder_has_an_argument`
/// fails the build if the schema and this list ever come apart.
pub const ACCOUNT_PLACEHOLDERS: [&str; 3] = ["username", "owner", "account"];

impl RequestBody {
    /// Whether `--data` applies.
    ///
    /// An empty list means the spec declared a `requestBody` without saying
    /// what goes in it. The CLI has always assumed JSON there, and still
    /// does — the alternative is withdrawing a flag that works today.
    pub fn accepts_json(&self) -> bool {
        self.content_types.is_empty() || self.content_types.iter().any(|ct| is_json(ct))
    }

    /// The media type a `--file` would be sent as: the first declared type
    /// that is not JSON. `None` when the body is JSON-only, which is what
    /// makes `--file` a flag those commands never show.
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

    /// The media type for a body that is text the caller types, not a file.
    ///
    /// `--data` carries it, unparsed: `starFile`'s whole body is the word
    /// `true`, and there is nothing to be gained by making that a file.
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

/// (service name, operationId, why) for operations this CLI declines to
/// expose.
///
/// Distinct from `UNSUPPORTED_OPERATIONS`, which lists what the platform
/// makes impossible. These would work; we choose not to offer them, so the
/// reason has to be written down or a later reader will "fix" the omission.
const WITHHELD_OPERATIONS: &[(&str, &str, &str)] = &[
    // "Lock or unlock a style from editing and deletion." Unlocking is the
    // dangerous half: it turns a protected style into a deletable one, and a
    // CLI makes that a single line with no confirmation.
    //
    // Verified 2026-09-08 against production with a real token holding
    // `styles:protect` (already registrable): the endpoint
    // works — `PUT .../protected` returned 200. It rejects a JSON body
    // (`{"protected":false}` → 400 "Must be plaintext true or false") and
    // wants the literal string `true`/`false` instead. So this is not
    // platform-blocked the way the admin-only style endpoints below are —
    // it is withheld purely for the safety reason above, not for lack of
    // access.
    (
        "styles",
        "setStyleProtected",
        "unlocks a style for deletion",
    ),
    // Admin-only, and gated on a role rather than a scope: a token from
    // `mapbox auth login` gets a bare 403 with no scope named, including for
    // an account that belongs to Mapbox.
    ("styles", "adminGetStyle", "admin-only endpoint"),
    ("styles", "adminUpdateStyle", "admin-only endpoint"),
    // 3D model assets are a different product surface from the glyph and
    // metadata endpoints the rest of `fonts` covers, and nothing here has
    // ever fetched one — no account reachable from this CLI has a model to
    // ask for, so the command would ship untested.
    //
    // Verified 2026-09-08: this is purely a test-data gap, not a scope
    // problem. A request for a nonexistent model against production returned
    // 404 "Model ... not found", not 403 — confirming the endpoint only
    // needs `fonts:read`, which `DEFAULT_SCOPES` already requests.
    ("fonts", "getModelAsset", "not supported yet"),
    // The v1-v3 API is dead to this CLI. `getLegacyTile` answers 410 for
    // every request, which the spec agrees is correct — it documents the
    // endpoint as deprecated and no longer supported. `getLegacyGrid`
    // rejects a token from `mapbox auth login` with a JSONP-wrapped 401,
    // for every version and every tileset, including the one whose grid
    // `getGrid` returns. Neither can succeed, so neither is a command.
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

/// Some commands get an ugly name because we build `command_name` from the
/// spec's own `operationId`, and once in a while that `operationId` is bad
/// — tilequery's names its one operation after its own URL path instead of
/// after what it does. We can't fix the spec, so this table gives that
/// command a better name to go by instead.
///
/// Table shape: `(service, operationId, alias, show_generated_name)`.
///   - `service` / `operationId`: which operation this is about — same key
///     `WITHHELD_OPERATIONS` and `BODY_CONTENT_TYPE_OVERRIDES` use.
///   - `alias`: the better name.
///   - `show_generated_name`: `true` keeps the generated name as the command
///     and makes the alias a second, equally visible way to write it.
///     `false` swaps them — the alias becomes the command, and the generated
///     name survives only as a hidden alias: it still runs, and nothing
///     names it anywhere, `--help`, `--schema` and `docs/commands.md`
///     alike.
///
/// Empty today. Its one row renamed tilequery's operation to `get-tilequery`,
/// and [`CLI_COMMAND_EXTENSION`] now names that operation `tilequery get`
/// directly — a renaming table beside a renaming extension is two answers to
/// one question. The mechanism stays because the extension cannot express
/// the other half of what a row does: keep a retired spelling running while
/// publishing it nowhere.
const COMMAND_ALIASES: &[(&str, &str, &str, bool)] = &[];

/// The OpenAPI extension that says where an operation's command belongs:
/// `x-mapbox-cli-command: [service, path_segment...]`.
///
/// A maintainer-only step writes it onto every operation it keeps, from a
/// per-operation decision record for what belongs in this CLI's command
/// surface. So `openapi/` carries the whole answer and nothing here has a
/// second opinion about it.
/// Two consequences nothing else in this file would lead you to expect:
///
///   - **The first element is the real service, and it need not be the one
///     the spec file is wired under.** `styles.yaml`'s sprite operations name
///     `sprites`; the one operation in `vectortiles.yaml` names `tilesets`.
///     A service in [`MAPBOX_SPEC_ENTRIES`] can therefore end up with no
///     operations at all, and a service nothing wires can end up with five.
///     [`regroup_by_service`] is where that happens, and it is the reason
///     `parse_spec`'s answer is an intermediate rather than the surface.
///   - **The rest is a path, not a name.** More than one segment nests:
///     `["draft", "get"]` puts the command under an intermediate `draft`
///     group, as `mapbox styles draft get`.
///
/// Absent from everything in `custom-openapi/`, which the decision record
/// does not cover. Those keep the pre-extension behaviour: their file's own
/// service name, and one flat generated command name.
const CLI_COMMAND_EXTENSION: &str = "x-mapbox-cli-command";

/// Where [`CLI_COMMAND_EXTENSION`] says this operation's command goes, as
/// (service, command path). `None` for an operation that declares none.
///
/// A declared-but-unusable value is an error rather than a fallback. It can
/// only come from a bug in the strip step, and the quiet reading of it —
/// keeping the file's own service and the generated name — is a command that
/// silently appears under the wrong service, which is exactly the failure
/// this extension exists to prevent.
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
/// name it stands in for should keep showing up alongside it.
///
/// A row whose `service` or `operationId` matches nothing answers `None` for
/// every operation, and a dead row looks exactly like a working one from
/// here — so `every_command_alias_names_a_real_operation` checks the other
/// end, that each row actually reaches an operation.
fn alias_for(service_name: &str, operation_id: &str) -> Option<(&'static str, bool)> {
    COMMAND_ALIASES
        .iter()
        .find(|(svc, op, _, _)| *svc == service_name && *op == operation_id)
        .map(|(_, _, alias, show_generated)| (*alias, *show_generated))
}

/// What an operation ends up called, given the name its `operationId`
/// generated and whatever [`COMMAND_ALIASES`] says about it: the command
/// name, its visible aliases, and its hidden ones.
///
/// Split out of `parse_spec` so both answers can be tested without a row in
/// the real table standing for them — today every row says `false`, which
/// would leave the `true` arm running nowhere.
fn command_names(
    generated_name: String,
    alias: Option<(&'static str, bool)>,
) -> (String, Vec<&'static str>, Vec<String>) {
    match alias {
        // The generated name stays the command; the alias is a second
        // spelling `--help` lists beside it.
        Some((alias, true)) => (generated_name, vec![alias], vec![]),
        // The alias *is* the command, and the generated name goes quiet.
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

    /// Whether this is a service's own liveness probe rather than something
    /// a person or an agent would ask for.
    ///
    /// Every spec that has one puts it at the service root or at a
    /// conventional health-check path, and matching on the path catches all
    /// five — two of them have no `operationId` at all, so their command
    /// names are generated from a summary and cannot be matched by name.
    ///
    /// Excluded for a different reason from `UNSUPPORTED_OPERATIONS`: these
    /// are not impossible, they are simply not this CLI's business. Three of
    /// the five answer 404 in production anyway.
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
    /// One predicate rather than three repeated conditions, because two
    /// places have to agree on it exactly: the command tree
    /// (`build_service_command`) and the schema (`crate::schema`). An
    /// operation described but not runnable, or runnable but not described,
    /// is worse than one that is neither.
    pub fn is_exposed(&self) -> bool {
        self.disabled_scope.is_none() && !self.is_liveness_probe() && !self.is_withheld()
    }

    /// Whether running this operation changes something on Mapbox's side —
    /// which is what earns it a `--dry-run`.
    ///
    /// The HTTP method is the answer, and deliberately the only one. It is
    /// the single signal every spec carries, so the rule stays correct for a
    /// service nobody has looked at and for the next one that lands in
    /// `openapi-specs`; a hand-kept list of mutating operation IDs would go
    /// stale on the first sync. The cost is that a POST which only reads —
    /// `batchGeocode` posts a query and gets answers back — is offered a
    /// `--dry-run` it does not need. That is a spare flag on a handful of
    /// commands, against the alternative of a missing one on a `delete`.
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
    /// The whole command minus `mapbox`: `styles list`. Service included,
    /// because a pair can straddle two services now — the operations are
    /// paired by URL path, and `x-mapbox-cli-command` is free to file the two
    /// ends of one path under different services.
    pub command: String,
    /// The listing's own path. It is the detail operation's path less its
    /// last segment, but carrying it means a caller can see which of the
    /// failing operation's path parameters the listing needs as well —
    /// `/styles/v1/{username}/{style_id}/sprite` still wants a style — and
    /// does not have to re-derive an invariant this pass already knows.
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

/// (service name, operationId, scope) for operations whose spec-documented required
/// scope is absent from the Accounts API's registration allowlist,
/// audited 2026-08-28. Re-check before removing an entry — these can't be
/// fixed from this side: the fix is Mapbox making the scope registrable, and
/// an entry comes off this list once a direct `POST /oauth/register` grants
/// it back unchanged.
///
/// `fonts:list` and `fonts:write` (and therefore `listFonts`, `uploadFont`,
/// `deleteFont` and `updateFontMetadata`) came off this list once both
/// became registrable on 2026-09-08. `DEFAULT_SCOPES` in `auth.rs` requests
/// them now, so all four commands ship. `updateFontMetadata` briefly sat in
/// `WITHHELD_OPERATIONS` instead: a same-day retest with a `mapbox auth
/// login` token found `PATCH .../{face}/metadata` 404 "Font not found for
/// expected owner" against a font the same token could upload, read and
/// delete at that exact path. A later retest, same day, with a freshly
/// uploaded font and the same token, got 200 both ways (`visibility`
/// flipped and stuck, confirmed with `get-font-metadata --fresh`) — most
/// likely the scope's authorization hadn't finished propagating yet right
/// after the scopes became registrable. Moved back here rather than back to
/// `WITHHELD_OPERATIONS`, since the 404 was a propagation delay, not
/// something about the `tk` usage code.
const UNSUPPORTED_OPERATIONS: &[(&str, &str, &str)] = &[
    // Confirmed live 2026-09-08: `fonts:metadata` is a real scope name, not a
    // documentation typo — the endpoint answers 403 "This API requires a
    // token with fonts:metadata scope" verbatim. It just isn't in
    // the registration allowlist yet.
    ("fonts", "getFontCoverage", "fonts:metadata"),
    // tokens:write is not registrable either — confirmed by a direct
    // POST /oauth/register against production: it's silently dropped from the
    // granted scope even when requested via both body and query. It only exists
    // in the classic, role-gated (ADMIN-only) token-creation path, not DCR.
    ("accounts", "createToken", "tokens:write"),
    ("accounts", "updateToken", "tokens:write"),
    ("accounts", "deleteToken", "tokens:write"),
    // Found by running it, not by reading: the styles spec documents no
    // scope, and an early probe answered 403 "requires a token with
    // styles:download scope". `POST /oauth/register` then drops
    // `styles:download` from the granted set, so no login can carry it.
    //
    // Deeper still (verified 2026-09-08 with a real token, no scope
    // involved): the endpoint now answers 403 "This is a prerelease API.
    // Please contact support at help@mapbox.com to request access." —
    // access is gated per-account, not by OAuth scope at all. Making
    // `styles:download` registrable would not unblock this command by
    // itself; that's why it wasn't made registrable alongside the other two
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
    /// The spec's `type`, when it is one clap can check before we spend a
    /// request finding out. `boolean` is absent here because it is handled
    /// as a flag rather than a value.
    pub numeric: Option<Numeric>,
}

/// A numeric parameter's width, kept apart from the generic string case so
/// the value can still be handed to the query string as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numeric {
    Integer,
    Float,
}

/// One spec file's embedded OpenAPI YAML, with the name this repo files it
/// under, ready for [`parse_spec`].
///
/// `name` is no longer necessarily a service anyone can type.
/// [`CLI_COMMAND_EXTENSION`] decides that per operation, so this is the
/// file's own name: the fallback service for an operation that declares no
/// target, the key `WITHHELD_OPERATIONS` and its neighbours match on, and
/// what a maintainer-only drift check compares the two tables by. `maps` is the
/// clearest case — the entry is still called that, and the service it used
/// to produce is gone.
#[derive(Clone, Copy)]
pub struct SpecEntry {
    pub name: &'static str,
    pub yaml: &'static str,
}

/// Every spec file that comes from openapi-specs. Each `yaml` is pulled in
/// via `include_str!` from `openapi/`, a vendored copy this repo owns.
///
/// Not what the CLI builds commands from — see [`effective_services`], and
/// [`CLI_COMMAND_EXTENSION`] for why a file's name and a service's name are
/// two different things.
///
/// `openapi/` no longer mirrors upstream verbatim: every operation a
/// maintainer-only decision record doesn't mark `enabled` is stripped out
/// before this file ever sees it. `sources` isn't listed below for that
/// reason: every operation in it is disabled there — it is not exposed as
/// a command, and this repo drops it from the running CLI now rather than
/// carrying a command group with nothing in it — so the vendoring step no
/// longer writes a file for it at all.
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
        name: "rasterarrays",
        yaml: include_str!("../openapi/api-rasterarrays/rasterarrays.production.v1.yaml"),
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
/// `custom-openapi/<service>/openapi/<file>.yaml` — one level up, not two:
/// `include_str!("../custom-openapi/search/openapi/search.yaml")`.
///
/// For an API openapi-specs doesn't publish a usable spec for yet. A name
/// here wins over the same name in [`MAPBOX_SPEC_ENTRIES`]; delete the
/// override once upstream ships the service — a maintainer-only drift check
/// flags a name wired on both sides for exactly that reason.
pub const CUSTOM_SPEC_ENTRIES: &[SpecEntry] = &[SpecEntry {
    name: "search",
    yaml: include_str!("../custom-openapi/search/openapi/search.yaml"),
}];

/// The list the CLI actually generates commands from: [`MAPBOX_SPEC_ENTRIES`]
/// with each [`CUSTOM_SPEC_ENTRIES`] override swapped in, then the
/// custom-only services appended.
pub fn effective_spec_entries() -> Vec<SpecEntry> {
    merge_entries(MAPBOX_SPEC_ENTRIES, CUSTOM_SPEC_ENTRIES)
}

/// The services the CLI builds its command tree from: every wired spec
/// parsed, then re-bucketed by each operation's own target service.
///
/// One function because the two steps are not separable — see
/// [`regroup_by_service`] — and every caller wants the pair.
pub fn effective_services() -> Result<Vec<ServiceSpec>> {
    let parsed: Vec<ServiceSpec> = effective_spec_entries()
        .iter()
        .map(|entry| parse_spec(entry.name, entry.yaml))
        .collect::<Result<_>>()?;
    Ok(regroup_by_service(parsed))
}

/// Split out so the precedence rule is testable against small tables.
/// Custom wins by replacement, in place — appending would leave two entries
/// claiming one service name, which clap only refuses in a debug build.
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

/// A second entry for the same name would otherwise vanish silently — the
/// second one dropped by [`merge_entries`]'s `find`/filter with no trace of
/// which spec was discarded. Catches it in either table, before the merge
/// hides it.
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

/// Title and description for a service no spec file is wired under.
///
/// [`CLI_COMMAND_EXTENSION`] assembles these out of operations that live in
/// other files — `sprites` out of five of `styles.yaml`'s, `tilesets` out of
/// one each from `rastertiles.yaml` and `vectortiles.yaml` — so there is no
/// `info.title` left that describes either. Hand-written for that reason and
/// only that reason: a service that still owns a file keeps that file's
/// `info` exactly as before.
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
];

/// One line of help for an intermediate command group — a path segment
/// several operations share, which is a command in the tree with no
/// operation of its own behind it.
///
/// Hand-written because nothing describes it: the group exists only because
/// [`CLI_COMMAND_EXTENSION`] filed two commands under one word, and no spec
/// has anything to say about that word. Keyed by the whole path, service
/// included, which is how `crate::build_service_command` looks one up.
/// `every_command_group_is_described` holds the table to the groups the
/// surface really builds.
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
/// [`CLI_COMMAND_EXTENSION`] rather than from the file it was parsed out of,
/// and may name a service some *other* file is wired under — or one no file
/// is. Which operations a service has is therefore not knowable until every
/// file has been parsed, which is why `parse_spec` answers per file and the
/// surface is assembled here. Three things follow:
///
///   - **A service whose operations all moved away is dropped**, rather than
///     built as a command group with nothing in it. `maps` and `vectortiles`
///     are exactly that today: their one operation each now targets
///     `tilesets`. An empty group would be listed by `mapbox --help` and then
///     refused by its own `subcommand_required(true)`.
///   - **A service no file is wired under** takes its title and description
///     from [`MERGED_SERVICES`].
///   - **Order is first appearance**, across the files in
///     [`effective_spec_entries`] order, so a service sits where its
///     operations put it.
pub fn regroup_by_service(parsed: Vec<ServiceSpec>) -> Vec<ServiceSpec> {
    // Kept before `parsed` is consumed: a service that still owns a file
    // keeps that file's `info`, and the file is the only place it exists.
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
/// The `name`, `title` and `description` are the file's; the operations may
/// not be, since each one carries the service its own
/// [`CLI_COMMAND_EXTENSION`] names. [`regroup_by_service`] is what turns a
/// list of these into the services the CLI actually builds — nothing else
/// should treat one of these as a service.
pub fn parse_spec(service_name: &str, yaml: &str) -> Result<ServiceSpec> {
    let doc: Value = serde_yaml::from_str(yaml)
        .with_context(|| format!("Failed to parse YAML for service '{}'", service_name))?;

    let title = doc["info"]["title"]
        .as_str()
        .unwrap_or(service_name)
        .to_string();

    // Flattened to one line, unlike a parameter's: this renders as a
    // service's `long_about`, where the spec's own wrapping buys nothing.
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

        // Collect path-level parameters (shared across methods)
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

            // The extension has the last word on both where the command
            // lives and what it is called; the generated name is what is
            // left when a spec declares none. See `CLI_COMMAND_EXTENSION`.
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

            for p in op_params {
                // Auto-filled from the global `--username`; see
                // ACCOUNT_PLACEHOLDERS.
                if ACCOUNT_PLACEHOLDERS.contains(&p.name.as_str()) {
                    continue;
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
/// `Some` with an empty `content_types` is deliberate rather than `None`: a
/// `requestBody` with no `content` still means the operation takes a body,
/// which is exactly what the old `has_body` bool recorded, and
/// [`RequestBody::accepts_json`] keeps treating it as JSON.
fn parse_request_body(op: &Value, full_spec: &Value) -> Option<RequestBody> {
    let request_body = &op["requestBody"];
    // A shared body lives under `components/requestBodies` and arrives here
    // as a `$ref`. That is still a mapping, so the check below would pass it
    // through as a body with no declared type and no `required` — an
    // operation that insists on a body, described as taking an optional JSON
    // one. No bundled spec factors a body out today; one sync could.
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
/// Preferring a property whose schema is `format: binary` — directly or as an
/// array's items — rather than taking the first one: a multipart body can mix
/// files with ordinary text fields, and posting the bytes under the wrong
/// name is a 400 that names neither.
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
/// each of those back to the listing.
///
/// A detail operation is a `GET` whose path is the listing's path plus one
/// more parameter — `/styles/v1/{username}` and
/// `/styles/v1/{username}/{style_id}`. That extra parameter has to identify
/// an item, so the account-scoping ones are excluded: `/tokens/v2` extended
/// by `{username}` is another listing, not a way to look at one token, and
/// offering `mapbox accounts list-tokens <username>` as "see one of these"
/// would be a lie about what the API can do.
fn link_detail_operations(operations: &mut [Operation]) {
    // Only operations that are commands may be pointed at. The hint this
    // fills reaches a caller as something to run — `--schema` publishes it as
    // `detail_command` — and half the withheld and unusable sets are GETs on
    // exactly the paths this matches, so an unfiltered pairing would sooner
    // or later name a command that answers like a typo.
    let candidates: Vec<(String, String)> = operations
        .iter()
        .filter(|op| op.method == "GET" && op.is_exposed())
        .map(|op| (op.path_template.clone(), op.command()))
        .collect();

    for op in operations.iter_mut().filter(|op| op.method == "GET") {
        let prefix = format!("{}/{{", op.path_template);
        op.detail = candidates.iter().find_map(|(path, command)| {
            // The name comes from the path, not from the parameter list: the
            // path is what defines the extension, and some specs describe a
            // placeholder without declaring a parameter for it.
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

    // The same pairing read the other way, for the failure that wants it: a
    // 404 from a detail operation is most often an id that does not exist,
    // and the listing is where the ids that do exist come from. Only a
    // listing that is itself a command may be named — the pass above pairs on
    // paths alone, so an unexposed GET can hold a `detail` link while not
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
        // Matched on the whole command, service included, rather than on the
        // last word: `styles get` and `styles draft get` share one.
        let command = op.command();
        op.listing = listings
            .iter()
            .find(|(detail, _)| *detail == command)
            .map(|(_, listing)| listing.clone());
    }
}

/// Everything up to the first blank line, wrapped as the spec wrapped it.
///
/// Spec prose puts the definition first and the reference material after a
/// break — the options table, the per-country notes, the worked examples.
/// The first paragraph is the part that describes the thing.
///
/// The line breaks inside it are kept. They are what `--help` has always
/// rendered, so dropping them would reflow four arguments' help for no
/// reason, and they are the only structure the bulleted ones have.
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
    // The first paragraph, the same rule the service description above uses.
    //
    // It used to be cut at the first `.`, which is fine for a line of help
    // and wrong for `--schema`, where it is the field a caller reasons from:
    // that cut lands inside `username.tileset-id`, inside `(range -85.0511,
    // 85.0511)` and inside `e.g.`, publishing a truncated identifier and a
    // wrong bound. Keeping the whole text instead was the other extreme —
    // `geocoder --types` alone is 4.6 KB of feature-type reference, and the
    // schema is read on every call. A paragraph keeps every case the cut
    // broke and leaves the appendices behind. `crate::first_sentence`
    // shortens it further for help, which renders exactly as it always has.
    let description = val["description"].as_str().map(first_paragraph);

    let schema = &val["schema"];
    let type_str = schema["type"].as_str().unwrap_or("string");
    let is_boolean = type_str == "boolean";
    let numeric = match type_str {
        "integer" => Some(Numeric::Integer),
        "number" => Some(Numeric::Float),
        _ => None,
    };

    // Numbers and booleans count. Keeping only the strings silently dropped
    // `tilesize: enum [256, 512]`, the one non-string enum among the
    // parameters in the bundled specs — so the CLI accepted `300`, sent it,
    // and let the API refuse it. Everything downstream wants the value as
    // text anyway: it is going into a URL.
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

    /// A listing that is not itself a command must never be named as one.
    /// This fixture gives a listing-shaped operation an operationId that's
    /// in `UNSUPPORTED_OPERATIONS` (`getFontCoverage`, `fonts:metadata` —
    /// no login can carry that scope), so it's filtered out of the surface —
    /// and a 404 pointed at it would tell the caller to run something that
    /// does not exist. Which entry the fixture borrows doesn't matter; only
    /// that one still needs a scope DCR won't grant. `listFonts` served this
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
    /// survive parsing exactly — not be normalised into a guess.
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

    /// The command a merged entry would actually generate, which is the only
    /// form of "which spec won" that a caller can see.
    ///
    /// Named apart from the real `command_names` (which computes one
    /// operation's name and aliases): a glob `use super::*` would otherwise
    /// let this test-only helper shadow it.
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
        // alongside the generated ones — `auth` has no `COMMAND` const to
        // borrow, the rest do. A spec table claiming any of these names
        // collides the same way two same-named spec entries would, and
        // `merge_entries` has no way to catch a clash with a name outside
        // its own tables. Pulling from each module's `COMMAND` const, rather
        // than hard-coding the string a second time, means a seventh
        // hand-written command is covered the day it lands.
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
    /// `alias_for` answers `None` for a row whose `service` or `operationId`
    /// is misspelled, exactly as it does for the operations no row mentions —
    /// so the alias silently never appears, the command keeps its generated
    /// name, and nothing else notices. `tests/api_command_surface.rs` pins
    /// the surface as it is, which means a row that never worked is pinned
    /// as working. This is the check that a row does something.
    ///
    /// Against the effective (merged) specs rather than a fixture, because
    /// the thing that goes wrong is a row pointing at an `operationId` those
    /// specs do not have — either mistyped, or renamed upstream by a later
    /// sync.
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

            // The alias arrives as the command's own name or as one it
            // answers to, depending on the row — either way, exactly one
            // operation carries it, and none does if the row is dead.
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

    /// A merge with a literal empty custom table — not `CUSTOM_SPEC_ENTRIES`,
    /// which now has `search` in it — changes nothing about the mapbox table:
    /// not order, not content, not which names appear. The general case
    /// `a_mapbox_service_with_no_override_passes_through_untouched` above
    /// exercises with a real override; this is its degenerate edge, pinned on
    /// the shipped table itself.
    #[test]
    fn merging_an_empty_custom_table_changes_nothing() {
        let merged = merge_entries(MAPBOX_SPEC_ENTRIES, &[]);
        assert_eq!(names(&merged), names(MAPBOX_SPEC_ENTRIES));
    }

    /// The one duplicate shape `merge_entries` used to swallow with no
    /// trace: two `CUSTOM_SPEC_ENTRIES` entries for a name that also exists
    /// in `MAPBOX_SPEC_ENTRIES`. Both the first-match `find` and the
    /// unmatched-name filter treat this as an ordinary override, so the
    /// output has no repeated name and every mapbox name is present — the
    /// merged-list assertions above cannot see it. Only a table-level check
    /// catches it, which is what this exercises.
    #[test]
    #[should_panic(expected = "CUSTOM_SPEC_ENTRIES lists `search` more than once")]
    fn a_repeated_custom_name_panics_instead_of_silently_dropping_one() {
        merge_entries(
            &[entry("search", FROM_MAPBOX)],
            &[entry("search", FROM_CUSTOM), entry("search", FROM_MAPBOX)],
        );
    }

    /// Both halves of `show_generated_name`, which no row in the real table
    /// can cover: every row says `false` today, so the `true` arm would ship
    /// having never run.
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
}
