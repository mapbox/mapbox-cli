//! How a command's result reaches its caller.
//!
//! Two audiences read this CLI's output and they want opposite things: a
//! person at a terminal wants prose, a script or an agent wants something
//! `jq` can parse. `--output` picks between them, and its default — `auto` —
//! decides by asking whether stdout is a terminal.
//!
//! The split is by *stream*, not by mode: stdout carries the result and
//! nothing else, stderr carries progress, warnings and errors. That holds in
//! both modes, so `mapbox ... > out.json` is always a clean document and
//! never has a "Waiting for authorization..." line wedged into it.
//!
//! Errors go to stderr in both modes, JSON-shaped under `json`. Keeping them
//! off stdout means a consumer never has to tell a result from a failure by
//! inspecting it — the exit code already says which it got.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};

use anyhow::Result;
use clap::parser::ValueSource;
use clap::ArgMatches;
use serde_json::{json, Value};

/// `--id`'s arg id. Not `id`: see the comment where it is declared.
pub const FILTER_ARG: &str = "filter-id";

/// Picks one row out of a list response.
///
/// The Tokens API has no way to fetch one token by id, and neither do
/// several other listings — but the row is right there in the response the
/// listing already returned. Filtering it here is the difference between
/// "the data exists somewhere" and "you can see it", and it needs no query
/// language and no `jq` on the machine.
///
/// Matches on `id`, or on `name` when the rows are keyed that way instead.
/// A miss is an error rather than an empty result: asking for one row and
/// silently getting none reads like the row exists and is empty.
pub fn pick_row(value: &Value, wanted: &str) -> Result<Value> {
    let rows = value.as_array().ok_or_else(|| {
        CliError::new(
            "not_a_list",
            "`--id` only applies to a command that returns a list.",
        )
    })?;

    for key in ["id", "name"] {
        let found = rows
            .iter()
            .find(|row| row.get(key).and_then(Value::as_str) == Some(wanted));
        if let Some(row) = found {
            return Ok(row.clone());
        }
    }

    Err(CliError::new("not_found", format!("No row has the id `{wanted}`.")).into())
}

/// The `--output` arg's id, and its three accepted values.
pub const ARG: &str = "output";
pub const AUTO: &str = "auto";
pub const TEXT: &str = "text";
pub const JSON: &str = "json";
pub const ENV: &str = "MAPBOX_OUTPUT";

/// Code carried by an error that nothing has classified further.
pub const GENERIC_CODE: &str = "error";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Text,
    Json {
        /// Indented, because a person is reading it. JSON asked for at a
        /// terminal is being looked at; JSON in a pipe is being parsed, and
        /// there one document per line is the more useful promise. The bytes
        /// differ, the document does not.
        pretty: bool,
    },
}

impl Mode {
    /// Resolves a requested value against the terminal-ness of stdout.
    ///
    /// Takes that as an argument rather than probing: it is the only input
    /// that cannot be arranged in a test, and every interesting case here is
    /// a combination of the two.
    ///
    /// An unrecognized `requested` is treated as `auto`. Clap rejects those
    /// before they reach us for both the flag and `MAPBOX_OUTPUT`; the one
    /// caller that can pass one is [`Mode::early`], which reads argv and the
    /// environment itself, before clap has had a chance to complain.
    pub fn resolve(requested: &str, stdout_is_terminal: bool) -> Self {
        match requested {
            JSON => Mode::Json {
                pretty: stdout_is_terminal,
            },
            TEXT => Mode::Text,
            _ if stdout_is_terminal => Mode::Text,
            _ => Mode::Json { pretty: false },
        }
    }

    /// The mode for a parsed command line.
    ///
    /// `MAPBOX_OUTPUT` is read here rather than declared as clap's `.env()`
    /// on the arg, because clap would *validate* it: `export MAPBOX_OUTPUT=`
    /// is a common way to clear a variable, and under `.env()` it made every
    /// command — including the ones needed to recover — fail with a usage
    /// error. An unusable value earns a warning and `auto`, never a dead CLI.
    pub fn from_matches(matches: &ArgMatches) -> Self {
        let typed = matches.value_source(ARG) == Some(ValueSource::CommandLine);
        let requested = if typed {
            matches
                .get_one::<String>(ARG)
                .cloned()
                .unwrap_or_else(|| AUTO.to_string())
        } else {
            environment_request().unwrap_or_else(|| AUTO.to_string())
        };

        Self::resolve(&requested, std::io::stdout().is_terminal())
    }

    /// The mode for failures raised before clap has produced any matches —
    /// a spec that will not parse, or a command line clap rejects.
    ///
    /// An explicit `--output` has to win even then: a usage error is exactly
    /// the moment a caller who asked for one shape and got the other has no
    /// way to recover. So argv is read directly, by
    /// [`requested_in_argv`], in clap's own precedence: command line, then
    /// environment, then `auto`.
    pub fn early(argv: &[OsString]) -> Self {
        let requested = requested_in_argv(argv)
            .or_else(environment_request)
            .unwrap_or_default();
        Self::resolve(&requested, std::io::stdout().is_terminal())
    }

    pub fn is_json(self) -> bool {
        matches!(self, Mode::Json { .. })
    }
}

/// `MAPBOX_OUTPUT`, if it holds anything at all.
///
/// An unrecognized value is passed through to [`Mode::resolve`], which falls
/// back to `auto` — but it is worth saying so, since the caller plainly meant
/// something by it.
fn environment_request() -> Option<String> {
    let value = std::env::var(ENV).ok()?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return None;
    }
    if ![AUTO, TEXT, JSON].contains(&value.as_str()) {
        eprintln!(
            "Warning: {ENV}={value} is not one of {AUTO}, {TEXT}, {JSON} — falling back to {AUTO}."
        );
    }
    Some(value)
}

/// Reads `--output`'s value straight off argv.
///
/// A deliberately small re-implementation of one flag's parsing, used only
/// when clap has already refused to parse the line — never in place of it.
/// It accepts the spellings clap does (`--output json`, `--output=json`,
/// `-o json`, `-ojson`, `-o=json`) and stops at `--`, past which nothing is
/// ours. It does not understand short-flag groups (`-do json`), it skips a
/// non-UTF-8 argument rather than stopping at it, and it does not know that
/// everything after `tilesets-cli` belongs to the child. Every one of those
/// misreads resolves to `auto`, which is what reading nothing would have
/// given — and all three only arise on a line clap already rejected, where a
/// best guess at the caller's intent beats ignoring what they wrote.
fn requested_in_argv(argv: &[OsString]) -> Option<String> {
    let mut args = argv.iter().filter_map(|arg| arg.to_str());

    while let Some(arg) = args.next() {
        if arg == "--" {
            return None;
        }
        if arg == "--output" || arg == "-o" {
            return args.next().map(String::from);
        }
        if let Some(value) = arg
            .strip_prefix("--output=")
            .or_else(|| arg.strip_prefix("-o="))
            .or_else(|| arg.strip_prefix("-o"))
        {
            if !value.is_empty() && !value.starts_with('-') {
                return Some(value.to_string());
            }
        }
    }

    None
}

/// A failure with a machine-readable code.
///
/// Most errors in this crate are plain `anyhow`, and render under
/// [`GENERIC_CODE`]. This exists for the ones a caller may reasonably want to
/// branch on — an HTTP status, a missing login — where "read the message"
/// is not a workable contract.
///
/// One caveat if you extend it: [`emit_error`] renders `message` alone, so
/// `.context("while creating the style")` wrapped *around* a `CliError`
/// is dropped in both modes. Put the context in the message instead.
#[derive(Debug)]
pub struct CliError {
    pub code: String,
    pub message: String,
    /// HTTP status, when the failure came from an API response.
    pub status: Option<u16>,
    /// The upstream response body, when it parsed as JSON. Preserved because
    /// Mapbox APIs put detail there that the message alone drops.
    pub body: Option<Value>,
    /// The raw response body, when it was not JSON. The message is capped at
    /// a readable length, so without this an HTML error page from a proxy
    /// would be truncated with nowhere to read the rest.
    pub body_text: Option<String>,
    /// What to do about the failure, in prose. Rendered as a line under the
    /// error in text, and as a field in JSON so a caller can surface or act
    /// on it rather than parse advice out of the message.
    pub fix: Option<String>,
    /// Commands to run next — shell lines and nothing else, so a caller can
    /// execute one without reading it first. Empty when there is nothing
    /// concrete to suggest; the reasoning lives in `fix` either way.
    pub next_actions: Vec<String>,
    /// The documentation for what failed. Attached per service and per
    /// status by [`crate::remedy`], which is where the URLs live.
    pub docs: Vec<String>,
    /// The request id from the response that failed — see
    /// `executor::REQUEST_ID_HEADERS` for which header it comes from.
    ///
    /// Always in the `json` rendering, where a field costs a reader nothing.
    /// In `text` only for a 5xx, because that is the failure a person takes
    /// to support — a 404 on a mistyped style id is theirs to fix, and an id
    /// under it would be noise on the common case.
    pub request_id: Option<String>,
}

impl CliError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        CliError {
            code: code.into(),
            message: message.into(),
            status: None,
            body: None,
            body_text: None,
            fix: None,
            next_actions: Vec::new(),
            docs: Vec::new(),
            request_id: None,
        }
    }

    /// Attaches [`crate::remedy::Remedy`]'s advice, filling gaps rather
    /// than overwriting.
    ///
    /// Two layers contribute to one error and they know different things:
    /// the executor knows the status and the operation, `auth` knows where
    /// the token that just failed came from. They are complementary, not
    /// competing — `remedy::for_http` deliberately leaves a 401's `fix`
    /// empty because only `auth` can write it — so the second caller adds to
    /// the first's advice instead of replacing it.
    pub fn with_remedy(mut self, remedy: crate::remedy::Remedy) -> Self {
        if self.fix.is_none() {
            self.fix = remedy.fix;
        }
        for action in remedy.next_actions {
            if !self.next_actions.contains(&action) {
                self.next_actions.push(action);
            }
        }
        for url in remedy.docs {
            if !self.docs.contains(&url) {
                self.docs.push(url);
            }
        }
        self
    }

    /// A non-2xx API response. The message is taken from the body's
    /// `message` field — what every Mapbox API puts the human explanation in
    /// — falling back to the status line when there is no such field.
    pub fn http(status: u16, body_text: &str) -> Self {
        let parsed: Option<Value> = serde_json::from_str(body_text).ok();
        // A body of literal `null` is no more informative than no body, and
        // carrying it would print a bare `null` under the error. It is still
        // *valid* JSON, though, so it must not fall through to the raw-text
        // arm below — that would make the message the string "null".
        let body = parsed.clone().filter(|value: &Value| !value.is_null());

        let from_field = body
            .as_ref()
            .and_then(|b| b.get("message"))
            .and_then(Value::as_str);
        // The Tilesets API answers a wrong-account token with a bare
        // `"Not found"` — a JSON string, not an object. Without this arm the
        // one thing it said would survive only inside `body`.
        let from_string = body.as_ref().and_then(Value::as_str);
        let from_text = parsed.is_none().then_some(body_text);

        let message = from_field
            .or(from_string)
            .or(from_text)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(truncate_for_message)
            .unwrap_or_else(|| format!("Request failed with HTTP {status}"));

        let body_text = parsed
            .is_none()
            .then(|| body_text.trim())
            .filter(|text| !text.is_empty())
            .map(String::from);

        CliError {
            code: format!("http_{status}"),
            message,
            status: Some(status),
            body,
            body_text,
            fix: None,
            next_actions: Vec::new(),
            docs: Vec::new(),
            request_id: None,
        }
    }

    /// Records the response's request id for this failure.
    ///
    /// Takes the `Option` rather than a value so the caller hands over
    /// whatever the response had without a branch of its own.
    pub fn with_request_id(mut self, request_id: Option<String>) -> Self {
        self.request_id = request_id;
        self
    }

    /// The request id worth showing a person, as opposed to a program.
    ///
    /// A 5xx only. The server broke, nothing the reader typed will fix it,
    /// and this is what lets support find the request. Under a 404 on a
    /// mistyped id it would be a line of noise beneath an error the reader
    /// can already act on — so the `json` rendering carries it always and
    /// this decides the `text` one.
    fn support_request_id(&self) -> Option<&str> {
        self.request_id
            .as_deref()
            .filter(|_| self.status.is_some_and(|status| status >= 500))
    }
}

/// Caps a message taken from a response body.
///
/// An error page from a proxy or WAF in front of the API is HTML, not JSON,
/// and can run to kilobytes; under `json` that would all land on one line in
/// the single field a caller reads. The first line is the part that ever
/// says anything, and the whole body is still carried under `body_text`.
fn truncate_for_message(text: &str) -> String {
    const LIMIT: usize = 200;

    let first_line = text.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() <= LIMIT {
        return first_line.to_string();
    }
    let clipped: String = first_line.chars().take(LIMIT).collect();
    format!("{clipped}…")
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

/// Prints a command's result. The only thing that writes to stdout.
///
/// `text` is the prose a person should see; `json` the object a program
/// should. Both are built eagerly — every result here is small enough that
/// deferring one behind a closure costs more at the call site than it saves.
pub fn emit(mode: Mode, text: &str, json: Value) -> Result<()> {
    match mode {
        Mode::Text => write_stdout(text),
        Mode::Json { pretty } => write_stdout(&encode(&json, pretty)?),
    }
}

/// One document, indented or not.
fn encode(value: &Value, pretty: bool) -> Result<String> {
    Ok(if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    })
}

/// Prints an API response.
///
/// Under `json` it goes out as one compact line, untouched. Under `text` it
/// is rendered as a table or a field list when the response has a shape that
/// suits one, and pretty-printed otherwise.
///
/// `service` gates the exception — see [`list_rendering`]: `search`'s,
/// `geocoder`'s and `tilequery`'s GeoJSON render as a list instead. Every
/// other value takes the path it always has.
///
/// `page` is the note that this response is one page of several. It goes to
/// stderr **in both modes**, unlike the other notes here: the result is just
/// as incomplete under `json`, and the API's own answer cannot carry the
/// fact without wrapping it in an envelope this CLI has promised not to add.
pub fn emit_value(
    mode: Mode,
    value: &Value,
    footer: Option<&str>,
    service: Option<&str>,
    page: Option<&str>,
) -> Result<()> {
    if let Mode::Json { pretty } = mode {
        write_stdout(&encode(value, pretty)?)?;
        // The only thing `json` prints to stderr on a success. A consumer
        // reading stdout alone is unaffected; one that would otherwise
        // believe it had the whole list is told. Through `print_tips` like
        // every other note, so the one thing `json` says on stderr is not
        // also the one thing shaped differently.
        print_tips(page.map(String::from).as_slice());
        return Ok(());
    }

    match list_rendering(value, service).or_else(|| render_human(value)) {
        Some(rendered) => {
            write_stdout(&rendered.text)?;
            // Advice about the result, so stderr — a `-o text > file` keeps
            // the table alone, and a reader still sees where to go next.
            // A blank line first. These are notes about the table, not more
            // of it, and butted against the last row they read as one.
            let next = match (footer, &rendered.identifier) {
                (Some(command), _) => Some(format!("To see one row: {command}")),
                // Every listing can answer this, so none of them has to send
                // the reader to a tool that ships with no operating system.
                // An earlier version suggested a `jq` pipeline here.
                (None, Some(example)) => Some(format!("To see one row: add `--id {example}`")),
                (None, None) => None,
            };

            // Every human rendering says where the machine one is. A table
            // that clipped something has a stronger reason to; a field list
            // shows every value already, so for it this is discoverability
            // rather than a warning, and the wording says which.
            let mut tips = vec![if rendered.shortened {
                "Values are shortened to fit; `-o json` prints each row whole.".to_string()
            } else {
                "`-o json` for the response as the API sent it.".to_string()
            }];
            tips.extend(next);
            // Last, because it is about the response as a whole rather than
            // about the rendering above it.
            tips.extend(page.map(String::from));
            print_tips(&tips);
            Ok(())
        }
        None => write_stdout(&serde_json::to_string_pretty(value)?),
    }
}

/// The list rendering a response earns, if it earns one.
///
/// An exact service match, never a "looks like GeoJSON" test: a new service
/// answering with a `FeatureCollection` keeps the pretty-JSON fallback until
/// somebody has looked at its features and decided what a line of them should
/// say. `None` — an unlisted service, no service at all, or a value that is
/// not a `FeatureCollection` — falls through to [`render_human`].
fn list_rendering(value: &Value, service: Option<&str>) -> Option<Rendered> {
    match service {
        Some("search") => match search_feature_rows(value) {
            Some(rows) => Some(render_feature_list(&rows)),
            None => render_category_table(value),
        },
        Some("geocoder") => {
            render_geocoder_list(value).or_else(|| render_batch_feature_list(value))
        }
        Some("tilequery") => tilequery_feature_rows(value).map(|rows| render_feature_list(&rows)),
        _ => None,
    }
}

/// A human rendering, what it had to cut, and the first identifier in it.
struct Rendered {
    text: String,
    shortened: bool,
    /// The first row's key, when the table has one — a real value for the
    /// `--id` suggestion, so the line can be copied and edited rather than
    /// filled in from scratch.
    identifier: Option<String>,
}

/// Widest a table column may start out, before `fit_to_line` narrows it.
const CELL: usize = 32;
/// Width a table aims to stay inside. Not the terminal's — std cannot ask —
/// but narrow enough to survive a split pane.
pub(crate) const LINE: usize = 100;
/// Spaces between columns.
const SEPARATOR: usize = 2;
/// Narrowest a column may be squeezed to before the table is left to wrap.
const FLOOR: usize = 8;

/// `search`'s `FeatureCollection`, as one row per result instead of GeoJSON.
///
/// `search forward`/`reverse`/`category` are asked for a list of POIs to
/// scan, the way any other listing is scanned — but as
/// [`render_feature_list`], not a table: see there for why. `geocoder` and
/// `tilequery` read the same way and reach the same renderer through their
/// own row-builders; every other service's GeoJSON stays pretty-printed — see
/// `shapes_a_table_would_misrepresent_are_left_alone` below — because it is a
/// handful of features whose nesting *is* the content a table would throw
/// away.
///
/// `None` for anything that is not a `FeatureCollection`, so a caller falls
/// back to rendering the original, untouched value.
fn search_feature_rows(value: &Value) -> Option<Vec<Value>> {
    let map = value.as_object()?;
    if map.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return None;
    }
    let features = map.get("features")?.as_array()?;
    let rows: Vec<Value> = features.iter().map(search_result_row).collect();
    if rows.iter().any(row_is_empty) {
        return None;
    }
    Some(rows)
}

/// `list-category`'s response as a table, with no `--id` suggestion.
///
/// `render_table` would offer one anyway, keyed on `name` — but `pick_row`
/// only accepts a bare top-level array, and `value` here is
/// `{"listItems": […], "attribution": …}`, an object. The suggestion would
/// be a command guaranteed to answer `not_a_list`, so it is dropped rather
/// than shown.
fn render_category_table(value: &Value) -> Option<Rendered> {
    let rows = search_category_rows(value)?;
    render_table(&rows).map(|rendered| Rendered {
        identifier: None,
        ..rendered
    })
}

/// `search list-category`'s response, as table rows.
///
/// Unlike a POI result, a category is three short strings —
/// `canonical_id` (what `search category` takes), `name` and `icon` — with
/// nothing long enough to need [`render_feature_list`]'s one-per-line
/// treatment, so this feeds the ordinary `columns_for` table instead.
/// `uuid` and `version` are left out: both are, per the docs, generated
/// fresh each request and "not intended for use as a persistent
/// identifier" — noise in a table meant to be read, not diffed.
///
/// `None` for anything without a `listItems` array (or one holding
/// something other than objects), so a caller falls back to the original
/// value — the same contract [`search_feature_rows`] gives.
fn search_category_rows(value: &Value) -> Option<Vec<Value>> {
    let items = value.as_object()?.get("listItems")?.as_array()?;
    items
        .iter()
        .map(|item| {
            let item = item.as_object()?;
            let mut row = serde_json::Map::new();
            if let Some(id) = item.get("canonical_id") {
                row.insert("canonical_id".to_string(), id.clone());
            }
            if let Some(name) = item.get("name") {
                row.insert("name".to_string(), name.clone());
            }
            if let Some(icon) = item.get("icon") {
                row.insert("icon".to_string(), icon.clone());
            }
            Some(Value::Object(row))
        })
        .collect()
}

/// A feature's position as `longitude,latitude` — the one piece all three
/// row-builders read the same way, so they read it here.
///
/// That order matches every coordinate parameter in this CLI (`--proximity`,
/// `--longitude`/`--latitude`), so the pair can be copied straight into one.
/// Taken off the GeoJSON geometry rather than `properties.coordinates`, which
/// puts latitude first and repeats the two numbers alongside fields
/// (`accuracy`, `routable_points`) a row has no room for.
///
/// `>= 2` because a GeoJSON position may legally carry a third element for
/// altitude; the first two are the pair either way.
fn coordinate_cell(feature: &Value) -> Option<Value> {
    let [lon, lat] = feature
        .get("geometry")
        .and_then(|g| g.get("coordinates"))
        .and_then(Value::as_array)
        .filter(|c| c.len() >= 2)
        .map(|c| [&c[0], &c[1]])?;
    Some(Value::String(format!("{lon},{lat}")))
}

/// One `search` result, cut down to what `search_feature_rows` keeps.
///
/// `Null` when there is no `properties` object to read at all — one of the
/// shapes [`row_is_empty`] has `search_feature_rows` fall back on.
fn search_result_row(feature: &Value) -> Value {
    let props = match feature.get("properties").and_then(Value::as_object) {
        Some(props) => props,
        None => return Value::Null,
    };

    let mut row = serde_json::Map::new();
    // `as_str` rather than a clone: `render_feature_list` reads every field
    // as a string and shows nothing at all for anything else, so a
    // wrong-typed value kept here would go missing without a word. Empty is
    // absent too, or the entry renders as a bare `1. `.
    if let Some(name) = props
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        row.insert("name".to_string(), Value::String(name.to_string()));
    }
    // The coordinates are why most callers run a search command at all —
    // the next thing to do with a result is almost always feed its location
    // somewhere else (`--proximity` on another search, a static map,
    // `search reverse`). See `coordinate_cell` for the shape.
    if let Some(coordinates) = coordinate_cell(feature) {
        row.insert("coordinates".to_string(), coordinates);
    }
    // `full_address` is the one meant to be read on its own — it already
    // concatenates `address` and `place_formatted` — so it wins when
    // present; `address` (street-level only) or `place_formatted`
    // (everything but the street) stand in for a result missing it.
    // `address` first of the two: one line of a POI's location is more use
    // as `15885 Dam Road` than as the city and country it sits in, which the
    // next result over most likely shares.
    //
    // `as_str` on each candidate rather than on the result, so a JSON `null`
    // — or anything else that is not a string — falls through to the next
    // one instead of ending the chain with a value nothing can render.
    if let Some(address) = props
        .get("full_address")
        .and_then(Value::as_str)
        .or_else(|| props.get("address").and_then(Value::as_str))
        .or_else(|| props.get("place_formatted").and_then(Value::as_str))
    {
        row.insert("address".to_string(), Value::String(address.to_string()));
    }
    if let Some(meters) = props.get("distance").and_then(Value::as_f64) {
        row.insert(
            "distance".to_string(),
            Value::String(format!("{:.1} km", meters / 1000.0)),
        );
    }
    // Answers "why did this match" — the thing a name and an address don't:
    // a `poi_category` search for `coffee` returning a bridge or a visitor
    // center is exactly the case where knowing *what kind of place* this is
    // matters as much as where it is. POI categories first, since they're
    // the more specific claim; `feature_type` (`poi`, `address`, `place`, …)
    // stands in for a result — an address, an administrative area — that
    // has no category at all.
    let category = props
        .get("poi_category")
        .and_then(Value::as_array)
        .filter(|categories| !categories.is_empty())
        .map(|categories| {
            categories
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .or_else(|| {
            props
                .get("feature_type")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if let Some(category) = category {
        row.insert("category".to_string(), Value::String(category));
    }
    Value::Object(row)
}

/// A `FeatureCollection`'s features, as one row per result — named
/// generically since `geocoder` is the first caller, not the last.
///
/// `search` and `tilequery` keep their own row-builders
/// ([`search_feature_rows`], [`tilequery_feature_rows`]) because what they
/// read out of a feature genuinely differs: a search result carries a
/// distance and a POI category a geocoding result has no equivalent of, and
/// a tilequery result carries whatever attributes its tileset happened to
/// put on the feature. All three hand their rows to the same
/// [`render_feature_list`], which is where the shapes do agree.
///
/// `None` for anything that is not a `FeatureCollection`, so the caller
/// falls back to the original value.
fn feature_collection_rows(value: &Value) -> Option<Vec<Value>> {
    let map = value.as_object()?;
    if map.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return None;
    }
    let features = map.get("features")?.as_array()?;
    let rows: Vec<Value> = features.iter().map(feature_row).collect();
    if rows.iter().any(row_is_empty) {
        return None;
    }
    Some(rows)
}

/// Whether a row has nothing in it to render.
///
/// `Null` is what all three row-builders — [`search_result_row`],
/// [`feature_row`], [`tilequery_row`] — answer for a feature with no
/// `properties` at all. An object with no fields is the same failure one step
/// later: `properties` was there and held nothing that builder recognized.
/// Both fall back the same way, because a numbered `(unnamed)` with no lines
/// under it looks like a result that is genuinely blank rather than like a
/// shape this renderer does not understand.
fn row_is_empty(row: &Value) -> bool {
    match row.as_object() {
        // The test is a field's shape, not its name: a field holding an empty
        // object says no more than an absent one, whichever field it is.
        Some(fields) => fields
            .values()
            .all(|value| value.as_object().is_some_and(serde_json::Map::is_empty)),
        None => true,
    }
}

/// A geocoding `FeatureCollection`, as its list with the notice under it.
fn render_geocoder_list(value: &Value) -> Option<Rendered> {
    let rows = feature_collection_rows(value)?;
    let mut rendered = render_feature_list(&rows);
    if let Some(notice) = attribution(value) {
        rendered.text.push_str(&format!("\n\n{notice}"));
    }
    Some(rendered)
}

/// A `FeatureCollection`'s `attribution`, when it carries a usable one.
///
/// Geocoding v6 requires the field on every response: it states the terms the
/// results come under, and `-o text` dropping it left the one legally
/// interesting line of the answer readable only under `-o json`.
///
/// Note the asymmetry with [`render_batch_feature_list`], which refuses to
/// render at all when a second key sits beside `batch`. There the extra key
/// would be *lost* by rendering only what is recognized, so bailing out to
/// JSON is how nothing goes missing; here the extra key is the thing being
/// carried through, so there is nothing to bail out for.
fn attribution(value: &Value) -> Option<&str> {
    value
        .get("attribution")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|notice| !notice.is_empty())
}

/// One feature, cut down to what [`feature_collection_rows`] keeps.
///
/// `Null` when there is no `properties` object — the shape
/// `feature_collection_rows` rejects as unexpected.
fn feature_row(feature: &Value) -> Value {
    let props = match feature.get("properties").and_then(Value::as_object) {
        Some(props) => props,
        None => return Value::Null,
    };

    let mut row = serde_json::Map::new();
    // An empty `name` is treated as absent, same as in `tilequery_row` —
    // otherwise the entry renders as a bare `1. ` with a trailing space.
    if let Some(name) = props
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        row.insert("name".to_string(), Value::String(name.to_string()));
    }
    if let Some(coordinates) = coordinate_cell(feature) {
        row.insert("coordinates".to_string(), coordinates);
    }
    // `full_address` first; `place_formatted` stands in when it's missing
    // (a bare place or region). `as_str` on each rather than on the result,
    // so an explicit JSON `null` — or anything else that is not a string —
    // falls through to the stand-in instead of landing in the row raw for
    // `render_feature_list` to drop without saying so.
    if let Some(address) = props
        .get("full_address")
        .and_then(Value::as_str)
        .or_else(|| props.get("place_formatted").and_then(Value::as_str))
    {
        row.insert("address".to_string(), Value::String(address.to_string()));
    }
    if let Some(feature_type) = props.get("feature_type").and_then(Value::as_str) {
        row.insert(
            "category".to_string(),
            Value::String(feature_type.to_string()),
        );
    }
    Value::Object(row)
}

/// `tilequery`'s features, as one row per result — same `feature_collection_rows`
/// contract, different fields: labeled layers (`poi_label`, `place_label`, a
/// named `road`, …) carry `properties.name`; unlabeled ones (`building`,
/// `landuse`, …) don't, and where the tileset sends `properties.type` as a
/// string that stands in. Not every tileset does — a raster-array feature has
/// neither — so a row with no name at all is a normal result, not a
/// malformed one. `properties.tilequery.layer` is the category and
/// `properties.tilequery.distance` the distance in meters; everything else
/// the tileset sent is carried under `extra` rather than dropped.
fn tilequery_feature_rows(value: &Value) -> Option<Vec<Value>> {
    let map = value.as_object()?;
    if map.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return None;
    }
    let features = map.get("features")?.as_array()?;
    let rows: Vec<Value> = features.iter().map(tilequery_row).collect();
    if rows.iter().any(row_is_empty) {
        return None;
    }
    Some(rows)
}

/// `Null` when there is no `properties` object — same contract as
/// [`feature_row`].
///
/// What a tileset puts in `properties` is its own business and nothing here
/// can know it in advance: a `mapbox-streets-v8` building carries `class`,
/// `height`, `min_height`, `extrude` and `underground`; a raster-array
/// feature carries `val` — one number per band, so a list — with `band`,
/// `zoom` and `units` inside its `tilequery` object. So everything a line can
/// carry that was not *consumed* by a named field above goes into `extra`,
/// because the attribute a caller queried *for* is exactly the one that must
/// not vanish. Consumed is the test, not the key's name: `type` is held back
/// only when it actually stood in for a missing `name`, and shown as an
/// attribute when `name` won. [`fits_on_a_line`] draws the limit; what it
/// rejects is a `-o json` away.
///
/// The `tilequery` object's leftovers come in on dotted keys
/// (`tilequery.band`), the same shape `render_fields` flattens one level of
/// nesting onto — a tileset may carry a top-level `zoom` or `geometry` of its
/// own, and a bare key would let one quietly overwrite the other.
///
/// `extra` comes out alphabetically, since this crate's `serde_json` has no
/// `preserve_order` and a `Map` is a `BTreeMap`. Stable is what matters —
/// the source order is the API's, not the tileset author's.
fn tilequery_row(feature: &Value) -> Value {
    let props = match feature.get("properties").and_then(Value::as_object) {
        Some(props) => props,
        None => return Value::Null,
    };

    let mut row = serde_json::Map::new();
    // `name` is the human label where a layer has one (poi_label, place_label, a
    // named road, …); layers with no notion of a name (building, landuse, …)
    // never send the field, so falling through to `type` — the specific type,
    // e.g. `building:part` — never displaces a real name. Not every tileset the
    // caller can name is `mapbox-streets-v8`, so `type` is checked for `str`
    // rather than assumed: a vector tile attribute is a tag with no schema
    // behind it and can be any MVT value type.
    let label = props
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    // Read only when `name` gave nothing, so that `type` is *consumed* here
    // exactly when it is shown here. Everything not consumed is an attribute
    // like any other and belongs in `extra` below.
    let stand_in = match label {
        Some(_) => None,
        None => props.get("type").and_then(Value::as_str),
    };
    if let Some(name) = label.or(stand_in) {
        row.insert("name".to_string(), Value::String(name.to_string()));
    }
    if let Some(coordinates) = coordinate_cell(feature) {
        row.insert("coordinates".to_string(), coordinates);
    }
    let tilequery = props.get("tilequery").and_then(Value::as_object);
    if let Some(layer) = tilequery
        .and_then(|t| t.get("layer"))
        .and_then(Value::as_str)
    {
        row.insert("category".to_string(), Value::String(layer.to_string()));
    }
    if let Some(meters) = tilequery
        .and_then(|t| t.get("distance"))
        .and_then(Value::as_f64)
    {
        row.insert(
            "distance".to_string(),
            Value::String(format!("{meters:.1} m")),
        );
    }

    // A key is held back only where it was actually used above. `type` is the
    // one that bites: a `poi_label` feature carries both `name` ("Bangkok9")
    // and `type` ("Restaurant"), and once `name` wins the label, `type` is a
    // plain attribute like any other — holding it back anyway dropped the
    // category, which is half of what a POI result says.
    let consumed = |key: &str| match key {
        // A string `name` either became the label or was empty and said
        // nothing; anything else was never usable as one and stays an
        // attribute, so that a numeric `name` is shown rather than swallowed.
        "name" => props.get("name").is_some_and(Value::is_string),
        "type" => stand_in.is_some(),
        "tilequery" => tilequery.is_some(),
        _ => false,
    };
    let mut extra: serde_json::Map<String, Value> = props
        .iter()
        .filter(|(key, value)| !consumed(key) && fits_on_a_line(value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    // The `tilequery` object's own leftovers — `band`, `zoom`, `units` on a
    // raster-array result — sit a level down and read the same way as the
    // top-level ones, so they are flattened in beside them, on the dotted
    // keys `render_fields` already flattens one level of nesting onto.
    // Qualifying them is not cosmetic: a tileset is free to carry a top-level
    // attribute named `geometry` or `zoom` too, and a bare key let one
    // silently overwrite the other on the way in.
    if let Some(tilequery) = tilequery {
        extra.extend(
            tilequery
                .iter()
                .filter(|(key, value)| {
                    !matches!(key.as_str(), "layer" | "distance") && fits_on_a_line(value)
                })
                .map(|(key, value)| (format!("tilequery.{key}"), value.clone())),
        );
    }
    if !extra.is_empty() {
        row.insert("extra".to_string(), Value::Object(extra));
    }

    Value::Object(row)
}

/// A `FeatureCollection`'s results, numbered: name, category and distance on
/// one line, the address on the next, coordinates on the one after, then one
/// `key: value` line per `extra` field.
///
/// Not a table. A table's column is a fixed width shared by every row, and a
/// street address is exactly the field that width can't be chosen for: narrow
/// enough to fit `--limit 10` results in 100 columns and it clips almost
/// every address to a few characters and an ellipsis, which is a worse answer
/// than the table not existing. A list costs a blank line between results
/// instead, and shows every name and every address whole.
///
/// One renderer for all three services that get a list. Which fields a row
/// carries is its own row-builder's business, and every field here is skipped
/// when absent: `extra` is `tilequery`'s alone, and `distance` arrives
/// already formatted — `km` from `search`, `m` from `tilequery` — so the unit
/// is the builder's decision rather than this function's.
fn render_feature_list(rows: &[Value]) -> Rendered {
    if rows.is_empty() {
        return Rendered {
            text: "(none)".to_string(),
            shortened: false,
            identifier: None,
        };
    }

    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.push_str("\n\n");
        }
        let name = row
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)");
        out.push_str(&format!("{}. {name}", index + 1));
        if let Some(category) = row.get("category").and_then(Value::as_str) {
            out.push_str(&format!(" ({category})"));
        }
        if let Some(distance) = row.get("distance").and_then(Value::as_str) {
            out.push_str(&format!(" — {distance}"));
        }
        if let Some(address) = row.get("address").and_then(Value::as_str) {
            out.push_str(&format!("\n   {address}"));
        }
        if let Some(coordinates) = row.get("coordinates").and_then(Value::as_str) {
            out.push_str(&format!("\n   {coordinates}"));
        }
        // Only `tilequery` fills this in; a geocoding row never carries it.
        if let Some(extra) = row.get("extra").and_then(Value::as_object) {
            for (key, value) in extra {
                let rendered = match value {
                    Value::Array(items) => join_list(items),
                    scalar => cell(Some(scalar)),
                };
                out.push_str(&format!("\n   {key}: {rendered}"));
            }
        }
    }

    // Never clipped, so nothing to warn about; no column to suggest `--id`
    // against either.
    Rendered {
        text: out,
        shortened: false,
        identifier: None,
    }
}

/// `batch-geocode`'s `{"batch": [FeatureCollection, …]}`, one query's list
/// per block, each numbered from 1. `None` unless every entry is a
/// `FeatureCollection` `feature_collection_rows` accepts — one malformed
/// query falls the whole batch back to the untouched value, same as
/// `feature_collection_rows` itself does for one malformed feature.
fn render_batch_feature_list(value: &Value) -> Option<Rendered> {
    let map = value.as_object()?;
    if map.len() != 1 {
        return None;
    }
    let queries = map.get("batch")?.as_array()?;
    let lists: Vec<Vec<Value>> = queries
        .iter()
        .map(feature_collection_rows)
        .collect::<Option<_>>()?;

    let mut out = String::new();
    for (index, rows) in lists.iter().enumerate() {
        if index > 0 {
            out.push_str("\n\n");
        }
        // One query needs no header: there is nothing to tell it apart from.
        if lists.len() > 1 {
            out.push_str(&format!("Query {}:\n", index + 1));
        }
        out.push_str(&render_feature_list(rows).text);
    }

    // Every entry carries its own `attribution` and it is the API's terms
    // rather than the query's, so all fifty of them say the same thing. Once
    // under the whole batch, then — the same notice repeated under every
    // block would bury the results it belongs to. Two that genuinely differ
    // both survive, in the order they arrived.
    let mut notices: Vec<&str> = Vec::new();
    for notice in queries.iter().filter_map(attribution) {
        if !notices.contains(&notice) {
            notices.push(notice);
        }
    }
    for notice in notices {
        out.push_str(&format!("\n\n{notice}"));
    }

    Some(Rendered {
        text: out,
        shortened: false,
        identifier: None,
    })
}

/// Renders a response for a person, or gives up.
///
/// Deliberately driven by the response in hand rather than by the spec that
/// described it: the shape is right there at runtime, it needs no `$ref`
/// resolution, and it works the same for a service nobody has looked at.
/// Giving up is a real answer — GeoJSON, a bare value, or rows with nothing
/// in common all read better as JSON than as a table pretending they fit.
fn render_human(value: &Value) -> Option<Rendered> {
    match value {
        Value::Array(rows) => render_table(rows),
        Value::Object(map) => {
            // A listing the API wrapped in a one-key object is still a
            // listing: `{"path":[…]}`, `{"icons":[…]}`. Without this it falls
            // all the way through to pretty-printed JSON — one style's
            // iconset is 3 MB that way, against 440 lines as a table.
            if let Some(table) = wrapped_list(map).and_then(render_table) {
                return Some(table);
            }
            // Field lists never clip: they have the room.
            render_fields(value).map(|text| Rendered {
                text,
                shortened: false,
                identifier: None,
            })
        }
        _ => None,
    }
}

/// The rows of a listing that arrived inside a one-key object.
///
/// Only one key: two of them (`{"files":[…],"folders":[…]}`) are two listings,
/// and picking one to show would hide the other.
fn wrapped_list(map: &serde_json::Map<String, Value>) -> Option<&[Value]> {
    if map.len() != 1 {
        return None;
    }
    map.values().next()?.as_array().map(Vec::as_slice)
}

/// A table, when the array really is rows of like things.
fn render_table(rows: &[Value]) -> Option<Rendered> {
    if rows.is_empty() {
        return Some(Rendered {
            text: "(none)".to_string(),
            shortened: false,
            identifier: None,
        });
    }
    let objects: Vec<&serde_json::Map<String, Value>> =
        rows.iter().filter_map(Value::as_object).collect();
    if objects.len() != rows.len() {
        return None;
    }

    let columns = columns_for(&objects);
    if columns.is_empty() {
        return None;
    }

    let mut widths: Vec<usize> = columns
        .iter()
        .map(|name| {
            objects
                .iter()
                .map(|row| table_cell(row.get(name)).chars().count())
                .chain(std::iter::once(name.chars().count()))
                .max()
                .unwrap_or(0)
                .min(CELL)
        })
        .collect();
    fit_to_line(&columns, &mut widths);

    let mut out = String::new();
    let header: Vec<String> = columns.iter().map(|c| c.to_uppercase()).collect();
    out.push_str(&join_row(&header, &widths));

    let mut shortened = false;
    for row in &objects {
        let cells: Vec<String> = columns.iter().map(|c| table_cell(row.get(c))).collect();
        shortened |= cells
            .iter()
            .zip(&widths)
            .any(|(text, width)| text.chars().count() > *width);
        out.push('\n');
        out.push_str(&join_row(&cells, &widths));
    }

    // A table is for scanning and for copying an identifier out of; it is not
    // where you read a value in full. Say where that is, once, and only when
    // something was actually clipped — on stderr, because it is advice about
    // the result rather than part of it.
    let identifier = columns
        .first()
        .filter(|name| matches!(name.as_str(), "id" | "name"))
        .and_then(|name| Some(objects.first()?.get(name)?.as_str()?.to_string()));

    Some(Rendered {
        text: out,
        shortened,
        identifier,
    })
}

/// A single object as aligned `name  value` lines.
///
/// One level of nesting is flattened onto dotted keys, because dropping it
/// loses the answer: `accounts retrieve-token` puts everything worth reading
/// inside `token`, and a scalars-only view rendered the whole response as
/// `code  TokenValid`. Deeper than that, or an array, and the structure is
/// the information — those fall back to JSON.
fn render_fields(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    let mut fields: Vec<(String, String)> = Vec::new();

    for (key, value) in object {
        match value {
            v if is_scalar(v) => fields.push((key.clone(), cell(Some(v)))),
            Value::Array(items) if items.iter().all(is_scalar) => {
                fields.push((key.clone(), join_list(items)))
            }
            Value::Object(nested) => {
                for (inner, v) in nested {
                    let name = format!("{key}.{inner}");
                    match v {
                        v if is_scalar(v) => fields.push((name, cell(Some(v)))),
                        Value::Array(items) if items.iter().all(is_scalar) => {
                            fields.push((name, join_list(items)))
                        }
                        _ => return None,
                    }
                }
            }
            _ => return None,
        }
    }

    if fields.is_empty() {
        return None;
    }

    let width = fields.iter().map(|(k, _)| k.chars().count()).max()?;
    let lines: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("{k:width$}  {v}"))
        .collect();
    Some(lines.join("\n"))
}

/// A list of scalars on one line. One row of a table cannot afford this, but
/// a field list can — and `retrieve-token`'s thirteen scopes are the answer
/// to the question that command is usually asked.
fn join_list(items: &[Value]) -> String {
    if items.is_empty() {
        return "(none)".to_string();
    }
    let rendered: Vec<String> = items.iter().map(|v| cell(Some(v))).collect();
    let joined = rendered.join(", ");
    if joined.chars().count() <= LINE {
        return joined;
    }

    let mut shown = String::new();
    let mut used = 0usize;
    for (i, item) in rendered.iter().enumerate() {
        if used + item.chars().count() + 2 > LINE.saturating_sub(12) {
            return format!("{shown}… (+{} more)", rendered.len() - i);
        }
        if !shown.is_empty() {
            shown.push_str(", ");
            used += 2;
        }
        shown.push_str(item);
        used += item.chars().count();
    }
    shown
}

/// The columns worth showing.
///
/// Three rules, each earned on a real response — `accounts list-tokens` for
/// one account returns ninety-six rows and eleven fields, and a table of all
/// eleven is wider than any terminal and mostly noise:
///
/// - **Most rows must have it.** Three of those ninety-six carry a `token`,
///   and a column blank for the other ninety-three earns none of the width
///   it costs. Tidiness, not secrecy — ask for `--usage pk` and every row
///   has a token, so every row shows one.
/// - **It must vary.** `usage` is `sk` on every row and `default` is `no` on
///   every row. A column with one value in it says nothing a sentence
///   couldn't, and costs width the varying columns need.
/// - **It must not repeat another column.** `modified` equals `created` on
///   every row; showing both is the same date twice.
///
/// `id` and `name` lead because that is what a reader looks for first.
fn columns_for(rows: &[&serde_json::Map<String, Value>]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for row in rows {
        for (key, value) in row.iter() {
            if !is_scalar(value) {
                continue;
            }
            match counts.iter_mut().find(|(name, _)| name == key) {
                Some((_, n)) => *n += 1,
                None => counts.push((key.clone(), 1)),
            }
        }
    }

    let majority = rows.len().div_ceil(2);
    let mut columns: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n >= majority)
        .map(|(name, _)| name)
        .collect();

    columns.sort_by_key(|name| match name.as_str() {
        "id" => 0,
        "name" => 1,
        _ => 2,
    });

    // A single row has nothing to vary against, so both rules below would
    // strip it to nothing.
    if rows.len() > 1 {
        let column_of = |name: &str| -> Vec<String> {
            rows.iter().map(|row| table_cell(row.get(name))).collect()
        };

        let mut kept: Vec<String> = Vec::new();
        let mut seen: Vec<Vec<String>> = Vec::new();
        for name in columns {
            let values = column_of(&name);
            let varies = values.iter().any(|v| v != &values[0]);
            if varies && !seen.contains(&values) {
                seen.push(values);
                kept.push(name);
            }
        }
        columns = kept;
    }

    columns
}

/// Narrows a table until it fits, widest column first.
///
/// Trimming trailing columns instead would drop whatever the API happened to
/// put last, which on `list-tokens` is the note — the one field a person is
/// actually scanning for. Narrowing costs some characters of the longest
/// values and keeps every column; the untruncated values are a `-o json`
/// away. `FLOOR` stops a column shrinking past the point of carrying
/// anything, and a header never gets clipped.
fn fit_to_line(headers: &[String], widths: &mut [usize]) {
    let total = |w: &[usize]| w.iter().sum::<usize>() + SEPARATOR * w.len().saturating_sub(1);

    while total(widths) > LINE {
        let floors: Vec<usize> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                // An identifier is for copying — half of one is no use at
                // all, so it keeps its width and the other columns pay.
                if i == 0 && matches!(h.as_str(), "id" | "name") {
                    return widths[i];
                }
                h.chars().count().max(FLOOR)
            })
            .collect();

        let widest = widths
            .iter()
            .enumerate()
            .filter(|(i, w)| **w > floors[*i])
            .max_by_key(|(_, w)| **w)
            .map(|(i, _)| i);

        match widest {
            Some(i) => widths[i] -= 1,
            // Everything is at its floor; a very wide table simply wraps.
            None => break,
        }
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

/// Whether one `key: value` line can carry a value honestly.
///
/// A scalar, or a list of them — tilequery's `val` is one number per band,
/// so a single-band query is a one-element array and dropping arrays would
/// have lost the answer itself. Anything deeper is structure, and structure
/// is what `-o json` is for.
fn fits_on_a_line(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().all(is_scalar),
        other => is_scalar(other),
    }
}

/// One value, rendered whole. Absent and null both read as `-`: a blank
/// leaves a reader wondering whether the value is empty or the field is.
///
/// Nothing is shortened here. A field list has room for the real value, and
/// the table clips to its own fitted column widths — doing it twice once
/// gave `expires  2026-09-01` for a token with hours left on it.
fn cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "-".to_string(),
        Some(Value::Bool(b)) => if *b { "yes" } else { "no" }.to_string(),
        Some(Value::String(s)) => s.to_string(),
        Some(other) => other.to_string(),
    }
}

/// A table cell: the value, with a timestamp reduced to its date.
fn table_cell(value: Option<&Value>) -> String {
    let text = cell(value);
    shorten_timestamp(&text)
}

/// `2026-06-10T09:10:53.850Z` reads as `2026-06-10`. The time is real
/// information, but not at a glance and not at 24 characters a column — the
/// full value is a `-o json` away.
fn shorten_timestamp(text: &str) -> String {
    let looks_iso = text.len() >= 20
        && text.as_bytes()[10] == b'T'
        && text.ends_with('Z')
        && text[..10].chars().all(|c| c.is_ascii_digit() || c == '-');

    if looks_iso {
        text[..10].to_string()
    } else {
        text.to_string()
    }
}

fn join_row(cells: &[String], widths: &[usize]) -> String {
    let padded: Vec<String> = cells
        .iter()
        .zip(widths)
        .map(|(text, width)| {
            let len = text.chars().count();
            if len > *width {
                let clipped: String = text.chars().take(width.saturating_sub(1)).collect();
                return format!("{clipped}…");
            }
            format!("{text}{}", " ".repeat(width - len))
        })
        .collect();
    padded.join(&" ".repeat(SEPARATOR)).trim_end().to_string()
}

/// Prints an API response body that did not parse as JSON.
///
/// Under `json` the body becomes a JSON string, which is a valid document on
/// its own — a caller piping into `jq` gets something parseable rather than
/// a syntax error, and the response really is just text.
pub fn emit_text_body(mode: Mode, body: &str) -> Result<()> {
    match mode {
        Mode::Text => write_stdout(body),
        Mode::Json { pretty } => write_stdout(&encode(&Value::String(body.to_string()), pretty)?),
    }
}

/// Progress and diagnostics. Always stderr, in both modes: this is not the
/// result, and a caller redirecting stdout must not collect it.
pub fn progress(message: &str) {
    eprintln!("{message}");
}

/// A `CliError` as the object `json` mode prints.
///
/// Split out from [`emit_error`] because it is the machine-readable contract
/// — a consumer branches on these keys — and a function returning a value
/// can be tested, where one that writes to stderr cannot.
fn error_payload(e: &CliError) -> Value {
    let mut obj = json!({ "code": e.code, "message": e.message });
    if let Some(status) = e.status {
        obj["status"] = json!(status);
    }
    // Same rule the text rendering uses: a body whose only key is `message`
    // has already been said, and repeating it makes a consumer wonder which
    // of the two to read.
    if let Some(body) = e.body.as_ref().filter(|b| adds_detail(b)) {
        obj["body"] = body.clone();
    }
    if let Some(text) = &e.body_text {
        obj["body_text"] = json!(text);
    }
    if let Some(fix) = &e.fix {
        obj["fix"] = json!(fix);
    }
    // Absent rather than empty. `[]` invites a consumer to wonder whether the
    // list was computed and came out empty, which is the question a missing
    // key already answers.
    if !e.next_actions.is_empty() {
        obj["next_actions"] = json!(e.next_actions);
    }
    if !e.docs.is_empty() {
        obj["docs"] = json!(e.docs);
    }
    // Here whatever the status, unlike the `text` rendering: a field costs a
    // consumer nothing to ignore, and a caller logging failures wants the id
    // on all of them, not only the ones a person would escalate.
    if let Some(request_id) = &e.request_id {
        obj["request_id"] = json!(request_id);
    }
    obj
}

/// Renders a failure to stderr.
///
/// Flat, not wrapped in an `{"error": …}` object. Under `json`, stderr never
/// carries anything else machine-readable and stdout never carries a failure
/// at all, so a key naming the shape would answer a question nothing can
/// ask. `jq .message` beats `jq .error.message` for the same reason there is
/// no `state` field: the streams already separate the two cases.
pub fn emit_error(mode: Mode, err: &anyhow::Error) {
    let cli = err.downcast_ref::<CliError>();

    if mode.is_json() {
        let payload = match cli {
            Some(e) => error_payload(e),
            // `{:#}` flattens anyhow's context chain into one line, so a
            // wrapped error keeps the context that explains it.
            None => json!({ "code": GENERIC_CODE, "message": format!("{err:#}") }),
        };
        // Serialising a `json!` object cannot fail; fall back rather than
        // panic while already on the error path.
        let pretty = matches!(mode, Mode::Json { pretty: true });
        let line = encode(&payload, pretty)
            .unwrap_or_else(|_| r#"{"code":"error","message":"unserialisable error"}"#.to_string());
        eprintln!("{line}");
        return;
    }

    match cli {
        Some(e) => {
            match e.status {
                Some(status) => eprintln!("Error: {} (HTTP {})", e.message, status),
                None => eprintln!("Error: {}", e.message),
            }
            // The message is only ever one field of the body; print the rest
            // when there is a rest, so a person loses nothing that the old
            // dump-the-body behavior showed them.
            if let Some(body) = e.body.as_ref().filter(|b| adds_detail(b)) {
                if let Ok(pretty) = serde_json::to_string_pretty(body) {
                    eprintln!("{pretty}");
                }
            }
            // Only worth repeating when the cap actually dropped something.
            if let Some(text) = e.body_text.as_ref().filter(|t| *t != &e.message) {
                eprintln!("{text}");
            }
            if let Some(fix) = &e.fix {
                eprintln!("Fix: {fix}");
            }
            if let Some(request_id) = e.support_request_id() {
                eprintln!("Request ID: {request_id} (quote this to Mapbox support)");
            }
            eprint_labelled("Next", &e.next_actions);
            eprint_labelled("Docs", &e.docs);
        }
        None => eprintln!("Error: {err:#}"),
    }
}

/// Prints one or more tips on stderr, in the one shape every command uses:
/// a single tip reads `Tip: …`; two or more get a `Tips:` header with each
/// one indented on its own line below it. Nothing for an empty list, so the
/// caller needs no guard. A blank line first — these are notes about
/// whatever was just printed, not more of it, and butted against the last
/// line they'd read as one.
fn print_tips(tips: &[String]) {
    if tips.is_empty() {
        return;
    }
    eprintln!();
    if let [tip] = tips {
        eprintln!("Tip: {tip}");
    } else {
        eprintln!("Tips:");
        for tip in tips {
            eprintln!("  {tip}");
        }
    }
}

/// A labeled group of lines on stderr. Nothing for an empty list, so the
/// caller needs no guard.
fn eprint_labelled(label: &str, values: &[String]) {
    for line in labelled_lines(label, values) {
        eprintln!("{line}");
    }
}

/// Labels the first line and aligns the rest under it.
///
/// `Next: mapbox styles list` reads as one thing; a second `Next:` on
/// the line below reads as two unrelated ones. Continuation lines are
/// indented to the label's width instead.
fn labelled_lines(label: &str, values: &[String]) -> Vec<String> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if index == 0 {
                format!("{label}: {value}")
            } else {
                format!("{:width$}  {value}", "", width = label.len())
            }
        })
        .collect()
}

/// Whether a response body carries anything past the message already shown.
fn adds_detail(body: &Value) -> bool {
    match body.as_object() {
        Some(map) => map.keys().any(|k| k != "message"),
        None => true,
    }
}

fn write_stdout(line: &str) -> Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(json: &str) -> Value {
        serde_json::from_str(json).expect("test fixture parses")
    }

    /// Two commands under one `Next:` have to read as two commands, not as
    /// one wrapped line and not as two unrelated labels.
    #[test]
    fn a_second_labelled_line_is_aligned_under_the_first() {
        let lines = labelled_lines(
            "Next",
            &[
                "mapbox styles list-styles".to_string(),
                "mapbox auth whoami".to_string(),
            ],
        );

        assert_eq!(
            lines,
            [
                "Next: mapbox styles list-styles",
                "      mapbox auth whoami"
            ]
        );
        assert!(labelled_lines("Next", &[]).is_empty());
    }

    #[test]
    fn an_array_of_like_objects_becomes_a_table() {
        let out = render_human(&rows(
            r#"[{"id":"a1","name":"First"},{"id":"b2","name":"Second"}]"#,
        ))
        .expect("a table")
        .text;

        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            ["ID  NAME", "a1  First", "b2  Second"]
        );
    }

    /// Every rule that keeps a column out, on one fixture: `usage` never
    /// varies, `modified` repeats `created`, and `token` is on one row in
    /// four.
    #[test]
    fn columns_that_say_nothing_are_left_out() {
        let mut fixture = Vec::new();
        for i in 0..4 {
            let token = if i == 0 { r#","token":"pk.x""# } else { "" };
            fixture.push(format!(
                r#"{{"id":"r{i}","created":"2026-01-0{i}T00:00:00Z","modified":"2026-01-0{i}T00:00:00Z","usage":"sk"{token}}}"#
            ));
        }
        let out = render_human(&rows(&format!("[{}]", fixture.join(","))))
            .expect("a table")
            .text;

        let header = out.lines().next().expect("a header");
        assert!(header.contains("ID"), "{header}");
        assert!(header.contains("CREATED"), "{header}");
        for absent in ["MODIFIED", "USAGE", "TOKEN"] {
            assert!(!header.contains(absent), "{absent} should be out: {header}");
        }
    }

    #[test]
    fn a_row_can_be_picked_out_of_a_list_by_id_or_name() {
        let list = rows(r#"[{"id":"a1","note":"first"},{"id":"b2","note":"second"}]"#);
        assert_eq!(pick_row(&list, "b2").unwrap()["note"], "second");

        let named = rows(r#"[{"name":"streets"},{"name":"dark"}]"#);
        assert_eq!(pick_row(&named, "dark").unwrap()["name"], "dark");
    }

    /// A miss is an error, not an empty result: asking for one row and
    /// silently getting none reads like the row exists and is empty.
    #[test]
    fn picking_reports_a_miss_and_a_shape_it_cannot_search() {
        let list = rows(r#"[{"id":"a1"}]"#);
        let missing = pick_row(&list, "nope").unwrap_err();
        assert_eq!(
            missing.downcast_ref::<CliError>().expect("a CliError").code,
            "not_found"
        );

        let single = rows(r#"{"id":"a1"}"#);
        let wrong_shape = pick_row(&single, "a1").unwrap_err();
        assert_eq!(
            wrong_shape
                .downcast_ref::<CliError>()
                .expect("a CliError")
                .code,
            "not_a_list"
        );
    }

    /// The identifier a table was keyed by, so the advice underneath names a
    /// real value instead of a placeholder the reader has to fill in.
    #[test]
    fn a_table_reports_the_identifier_it_showed() {
        let keyed = render_human(&rows(
            r#"[{"id":"a1","name":"First"},{"id":"b2","name":"S"}]"#,
        ))
        .expect("a table");
        assert_eq!(keyed.identifier.as_deref(), Some("a1"));

        let unkeyed =
            render_human(&rows(r#"[{"a":"1","b":"x"},{"a":"2","b":"y"}]"#)).expect("a table");
        assert_eq!(unkeyed.identifier, None);
    }

    #[test]
    fn an_identifier_keeps_its_width_while_the_rest_give_way() {
        let long = "c".repeat(25);
        let out = render_human(&rows(&format!(
            r#"[{{"id":"{long}","a":"{p}","b":"{p}","c":"{p}","d":"{q}"}},
                {{"id":"other","a":"x","b":"y","c":"z","d":"w"}}]"#,
            p = "p".repeat(40),
            q = "q".repeat(40)
        )))
        .expect("a table")
        .text;

        assert!(out.contains(&long), "the id was clipped: {out}");
        assert!(
            out.lines().all(|l| l.chars().count() <= LINE + 25),
            "line ran away: {out}"
        );
    }

    #[test]
    fn a_single_object_becomes_a_field_list_including_one_level_of_nesting() {
        let out = render_human(&rows(
            r#"{"code":"TokenValid","token":{"usage":"tk","scopes":["a","b"]}}"#,
        ))
        .expect("a field list")
        .text;

        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                "code          TokenValid",
                "token.scopes  a, b",
                "token.usage   tk"
            ]
        );
    }

    /// Several APIs wrap a listing in one key — `{"path":[…]}` from
    /// `get-breadcrumb`, `{"icons":[…]}` from `get-iconset`. The rows are
    /// what the caller asked for, so they get the table.
    #[test]
    fn a_listing_wrapped_in_one_key_still_becomes_a_table() {
        let out = render_human(&rows(
            r#"{"icons":[{"name":"tunnel","size":3},{"name":"bridge","size":4}]}"#,
        ))
        .expect("a table")
        .text;

        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            ["NAME    SIZE", "tunnel  3", "bridge  4"]
        );
    }

    /// Two keys are two listings, and showing one would hide the other.
    /// `styles list-files` returns `files` and `folders` together.
    #[test]
    fn two_wrapped_listings_are_left_to_json() {
        assert!(render_human(&rows(r#"{"files":[{"id":"a"}],"folders":[{"id":"b"}]}"#)).is_none());
    }

    /// The wrapper only applies when the rows are rows. A one-key object
    /// holding scalars is a record with one field, and the field list says
    /// so better than a failed table would.
    #[test]
    fn one_key_holding_scalars_stays_a_field_list() {
        let out = render_human(&rows(r#"{"scopes":["styles:read","fonts:read"]}"#))
            .expect("a field list")
            .text;

        assert_eq!(out.trim(), "scopes  styles:read, fonts:read");
    }

    /// A field list has room, so nothing is shortened there — the table is
    /// the only place a timestamp loses its time.
    #[test]
    fn only_the_table_shortens_a_timestamp() {
        let field = render_human(&rows(r#"{"expires":"2026-09-01T23:50:16.000Z"}"#))
            .unwrap()
            .text;
        assert!(field.contains("23:50:16"), "{field}");

        let table = render_human(&rows(
            r#"[{"id":"a","expires":"2026-09-01T23:50:16.000Z"},{"id":"b","expires":"2026-09-02T01:00:00.000Z"}]"#,
        ))
        .unwrap()
        .text;
        assert!(
            table.contains("2026-09-01") && !table.contains("23:50:16"),
            "{table}"
        );
    }

    /// Structure that a two-column view would throw away stays as JSON.
    #[test]
    fn shapes_a_table_would_misrepresent_are_left_alone() {
        assert!(render_human(&rows(
            r#"{"type":"FeatureCollection","features":[{"a":1}]}"#
        ))
        .is_none());
        assert!(render_human(&rows(r#"[1,2,3]"#)).is_none());
        assert!(render_human(&rows(r#""just a string""#)).is_none());
    }

    /// `render_human` itself never special-cases `search` — the test above
    /// still holds for a bare GeoJSON value. The carve-out lives one level
    /// up, in `search_feature_rows`, which only `emit_value` reaches for
    /// when the caller's `service` is `search`.
    #[test]
    fn search_extracts_one_row_per_feature_everything_else_does_not() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"properties":{"name":"Starbucks","feature_type":"poi","full_address":"1 Main St","distance":19568}},
                {"properties":{"name":"Peet's","feature_type":"poi","full_address":"2 Main St","distance":30021}}
            ]}"#,
        );

        let extracted = search_feature_rows(&fc).expect("a FeatureCollection extracts");
        let list = render_feature_list(&extracted).text;
        assert!(
            list.contains("1. Starbucks") && list.contains("2. Peet's"),
            "{list}"
        );

        // Not a FeatureCollection: nothing to extract, so the caller falls
        // back to rendering the original value untouched.
        assert!(search_feature_rows(&rows(r#"[{"name":"a"}]"#)).is_none());
        assert!(search_feature_rows(&rows(r#"{"scopes":["a"]}"#)).is_none());
    }

    /// A shape close enough to fool the `type` check but not the rest of it
    /// — one feature with no `properties` among otherwise-normal ones — has
    /// to fall back to the untouched value too, not panic partway through.
    /// `emit_value` has no other path once `search_feature_rows` says yes,
    /// so this is the one place that can catch a response `search` never
    /// actually sends but a spec change or a stub server might.
    #[test]
    fn a_feature_collection_search_cannot_parse_falls_back_whole() {
        let one_broken_feature = rows(
            r#"{"type":"FeatureCollection","features":[
                {"properties":{"name":"Starbucks"}},
                {"geometry":{"coordinates":[0,0]}}
            ]}"#,
        );
        assert!(search_feature_rows(&one_broken_feature).is_none());

        let features_not_an_array = rows(r#"{"type":"FeatureCollection","features":"nope"}"#);
        assert!(search_feature_rows(&features_not_an_array).is_none());

        let no_features_at_all = rows(r#"{"type":"FeatureCollection"}"#);
        assert!(search_feature_rows(&no_features_at_all).is_none());
    }

    /// An empty row is the same failure as a missing `properties` object one
    /// step later — the feature was there and nothing in it was recognized —
    /// and has to fall back the same way rather than print a numbered
    /// `(unnamed)` with nothing under it. `feature_collection_rows` and
    /// `tilequery_feature_rows` guard this; search was still checking only
    /// for `Null`.
    #[test]
    fn a_search_feature_with_nothing_recognisable_falls_back_to_json() {
        for properties in ["{}", r#"{"mapbox_id":"opaque-id-nobody-reads"}"#] {
            let fc = rows(&format!(
                r#"{{"type":"FeatureCollection","features":[{{"properties":{properties}}}]}}"#
            ));
            assert!(
                search_feature_rows(&fc).is_none(),
                "properties {properties} should fall back"
            );
        }

        // A position on its own is still something worth showing, so a
        // feature carrying one is not empty and still renders.
        let coordinates_only = rows(
            r#"{"type":"FeatureCollection","features":[{"geometry":{"coordinates":[0,0]},"properties":{}}]}"#,
        );
        assert_eq!(
            render_feature_list(&search_feature_rows(&coordinates_only).expect("a list")).text,
            "1. (unnamed)\n   0,0"
        );
    }

    /// `feature_row`'s rule, applied to the two fields `search_result_row`
    /// was still cloning in raw. A conforming search response always sends
    /// strings here, so this is defensive — but `render_feature_list` shows
    /// nothing at all for a non-string, which is the silent drop the rest of
    /// this work exists to close.
    #[test]
    fn a_search_row_drops_a_non_string_name_and_address() {
        let wrong_types = rows(
            r#"{"properties":{"name":42,"full_address":{"line1":"x"},"address":["y"],"place_formatted":7}}"#,
        );
        let row = search_result_row(&wrong_types);
        assert!(row.get("name").is_none(), "{row}");
        assert!(row.get("address").is_none(), "{row}");

        // The chain steps past a wrong-typed candidate rather than ending on
        // it, the same way it steps past an absent one.
        let null_full_address =
            rows(r#"{"properties":{"name":"a","full_address":null,"address":"15885 Dam Road"}}"#);
        assert_eq!(
            search_result_row(&null_full_address)["address"],
            "15885 Dam Road"
        );

        // And an empty `name` is absent, not a bare `1. ` with nothing after.
        let blank = rows(r#"{"properties":{"name":"","feature_type":"poi"}}"#);
        assert!(search_result_row(&blank).get("name").is_none());
    }

    /// `list-category`'s response is short flat objects, not POIs — a table
    /// suits it, and `uuid`/`version` are noise a table shouldn't spend
    /// columns on.
    #[test]
    fn list_category_extracts_table_rows_without_uuid_or_version() {
        let response = rows(
            r#"{"listItems":[
                {"canonical_id":"food_and_drink","icon":"fast-food","name":"Food and Drink","uuid":"71fed985-…","version":"25:6bd9…"},
                {"canonical_id":"lodging","icon":"lodging","name":"Lodging","uuid":"8de7b125-…","version":"25:6bd9…"}
            ],"attribution":"© 2026 Mapbox"}"#,
        );

        let extracted = search_category_rows(&response).expect("listItems extracts");
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted[0]["canonical_id"], "food_and_drink");
        assert_eq!(extracted[0]["name"], "Food and Drink");
        assert_eq!(extracted[0]["icon"], "fast-food");
        assert!(extracted[0].get("uuid").is_none());
        assert!(extracted[0].get("version").is_none());

        let table = render_table(&extracted).expect("a table").text;
        assert!(table.contains("CANONICAL_ID"), "{table}");
        assert!(
            table.contains("food_and_drink") && table.contains("Lodging"),
            "{table}"
        );

        // No `listItems` at all: nothing to extract, caller falls back.
        assert!(search_category_rows(&rows(r#"{"attribution":"x"}"#)).is_none());
        assert!(search_category_rows(&rows(r#"{"listItems":"nope"}"#)).is_none());
        assert!(search_category_rows(&rows(r#"{"listItems":[1,2,3]}"#)).is_none());
    }

    /// End to end through `emit_value`'s own branching, not just against the
    /// extraction helper: a `search` response with no `type`/`features` but
    /// a `listItems` array renders as the category table, and a shape that
    /// is neither still falls back to `render_human`.
    #[test]
    fn search_renders_a_category_list_as_a_table_not_a_feature_collection() {
        let response = rows(
            r#"{"listItems":[{"canonical_id":"food_and_drink","icon":"fast-food","name":"Food and Drink"}],"attribution":"© 2026 Mapbox"}"#,
        );
        assert!(search_feature_rows(&response).is_none());
        let extracted = search_category_rows(&response).expect("listItems extracts");
        let table = render_table(&extracted).expect("a table").text;
        assert!(table.contains("food_and_drink"), "{table}");

        // Neither shape: falls through to the generic renderer, same as any
        // other service's response would.
        assert!(search_category_rows(&rows(r#"{"scopes":["a"]}"#)).is_none());
    }

    /// `render_table` alone would suggest `--id <name>` here, the same as it
    /// does for any other table — but `pick_row` only accepts a bare
    /// top-level array, and this response is `{"listItems": […],
    /// "attribution": …}`, an object. That suggestion is a command
    /// guaranteed to fail, so `render_category_table` must not carry it.
    #[test]
    fn list_category_never_suggests_an_id_pick_row_cannot_use() {
        let response = rows(
            r#"{"listItems":[{"canonical_id":"food_and_drink","name":"Food and Drink"}],"attribution":"x"}"#,
        );
        let rendered = render_category_table(&response).expect("a table");
        assert_eq!(rendered.identifier, None);

        // The identifier really would exist if this went through the plain
        // `render_table` path unwrapped — confirming the suppression is what
        // is doing the work here, not some other reason it is absent.
        let extracted = search_category_rows(&response).expect("listItems extracts");
        assert_eq!(
            render_table(&extracted).expect("a table").identifier,
            Some("Food and Drink".to_string())
        );
    }

    /// The format the readability complaint against the table asked for:
    /// name and distance on one line, the whole address on the next, never
    /// clipped — checked end to end from a raw `FeatureCollection` through
    /// both rendering stages, not just against a hand-built row.
    #[test]
    fn a_search_list_never_clips_an_address() {
        let long_address =
            "15885 Very Long Street Name That Would Never Fit In A Table Column, Clearlake, California 95422, United States";
        let fc = rows(&format!(
            r#"{{"type":"FeatureCollection","features":[
                {{"properties":{{"name":"Starbucks","full_address":"{long_address}","distance":19568}}}}
            ]}}"#
        ));

        let extracted = search_feature_rows(&fc).unwrap();
        let list = render_feature_list(&extracted).text;

        assert_eq!(list, format!("1. Starbucks — 19.6 km\n   {long_address}"));
    }

    /// Pins docs/commands.md's three `search` Outputs examples to what the
    /// code actually renders for the exact JSON each example's own `-o json`
    /// column shows — a doc example missing the coordinates line (once true
    /// of `category`'s, caught only by hand) fails here mechanically instead
    /// of at the next reader who tries the command and gets a fourth line.
    #[test]
    fn search_outputs_examples_in_the_docs_match_the_real_render() {
        let forward = rows(
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-122.059627,37.56153],"type":"Point"},"properties":{"name":"34170 Gannon Terrace","mapbox_id":"{mapbox_id}","feature_type":"address","full_address":"34170 Gannon Terrace, Fremont, California 94555, United States","distance":20045}}]}"#,
        );
        assert_eq!(
            render_feature_list(&search_feature_rows(&forward).unwrap()).text,
            "1. 34170 Gannon Terrace (address) — 20.0 km\n   34170 Gannon Terrace, Fremont, California 94555, United States\n   -122.059627,37.56153"
        );

        let reverse = rows(
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-118.471584,34.023345],"type":"Point"},"properties":{"name":"1827 21st Street","feature_type":"address","full_address":"1827 21st Street, Santa Monica, California 90404, United States"}}]}"#,
        );
        assert_eq!(
            render_feature_list(&search_feature_rows(&reverse).unwrap()).text,
            "1. 1827 21st Street (address)\n   1827 21st Street, Santa Monica, California 90404, United States\n   -118.471584,34.023345"
        );

        let category = rows(
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"coordinates":[-122.6180785,38.9307594],"type":"Point"},"properties":{"name":"Starbucks","feature_type":"poi","brand":["Starbucks"],"poi_category":["café","coffee","coffee shop"],"full_address":"15885 Dam Road, Clearlake, California 95422, United States","distance":19568}}]}"#,
        );
        assert_eq!(
            render_feature_list(&search_feature_rows(&category).unwrap()).text,
            "1. Starbucks (café, coffee, coffee shop) — 19.6 km\n   15885 Dam Road, Clearlake, California 95422, United States\n   -122.6180785,38.9307594"
        );
    }

    /// The coordinates are the reason most callers run a search command at
    /// all — the next step with a result is almost always feeding its
    /// location somewhere else — so they're on the list even though nothing
    /// else here comes from `geometry` rather than `properties`.
    /// `longitude,latitude`, matching the order every coordinate parameter
    /// in this service already takes (`--proximity`, `--longitude`
    /// `--latitude`), not `properties.coordinates`'s `latitude` first.
    #[test]
    fn a_search_row_reads_coordinates_from_the_geometry_not_properties() {
        let feature = rows(
            r#"{
                "geometry": {"type": "Point", "coordinates": [-122.059627, 37.56153]},
                "properties": {
                    "name": "a",
                    "coordinates": {"longitude": 37.56153, "latitude": -122.059627}
                }
            }"#,
        );
        assert_eq!(
            search_result_row(&feature)["coordinates"],
            "-122.059627,37.56153"
        );

        // No geometry at all: nothing to show, not a made-up pair.
        let no_geometry = rows(r#"{"properties":{"name":"a"}}"#);
        assert!(search_result_row(&no_geometry).get("coordinates").is_none());
    }

    #[test]
    fn a_search_list_shows_coordinates_on_their_own_line() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[-122.059627,37.56153]},"properties":{"name":"a","full_address":"1 Main St"}}
            ]}"#,
        );
        let extracted = search_feature_rows(&fc).unwrap();
        let list = render_feature_list(&extracted).text;
        assert_eq!(list, "1. a\n   1 Main St\n   -122.059627,37.56153");
    }

    /// `render_search_list` was a byte-for-byte copy of `render_feature_list`
    /// minus the `extra` lines — which a search row never carries — so the
    /// two were merged. This pins that the survivor still renders a search
    /// result exactly as the deleted one did, on a row filling every field a
    /// search result can: name, category, distance, address and coordinates.
    #[test]
    fn a_search_row_renders_through_the_shared_list_and_carries_no_extra() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[{"geometry":{"coordinates":[-122.6180785,38.9307594]},"properties":{"name":"Starbucks","poi_category":["café","coffee"],"full_address":"15885 Dam Road, Clearlake, California 95422, United States","distance":19568}}]}"#,
        );

        let extracted = search_feature_rows(&fc).expect("a FeatureCollection extracts");
        assert!(
            extracted[0].get("extra").is_none(),
            "the `extra` path is inert for search: {}",
            extracted[0]
        );
        assert_eq!(
            render_feature_list(&extracted).text,
            "1. Starbucks (café, coffee) — 19.6 km\n   15885 Dam Road, Clearlake, \
             California 95422, United States\n   -122.6180785,38.9307594"
        );
    }

    /// One coordinate reader for all three row-builders, so a position reads
    /// the same way whichever service asked for it.
    ///
    /// `search`'s own copy filtered on `len() == 2` and so dropped the line
    /// entirely — the field its own comment calls the reason most callers run
    /// the command — for a position carrying altitude. Sharing the reader
    /// gives it the `>= 2` the other two already had.
    #[test]
    fn every_row_builder_reads_a_three_element_position_the_same_way() {
        let feature = rows(
            r#"{"geometry":{"coordinates":[-122.059627,37.56153,12.5]},
                "properties":{"name":"a","type":"b"}}"#,
        );

        for row in [
            search_result_row(&feature),
            feature_row(&feature),
            tilequery_row(&feature),
        ] {
            assert_eq!(row["coordinates"], "-122.059627,37.56153", "{row}");
        }

        // And nothing invented where there is no usable position to read.
        assert!(coordinate_cell(&rows(r#"{"properties":{"name":"a"}}"#)).is_none());
        assert!(coordinate_cell(&rows(r#"{"geometry":{"coordinates":[1]}}"#)).is_none());
    }

    /// The columns are hand-picked, not `columns_for`'s usual majority-scalar
    /// rule — this pins which ones survive and which don't, since nothing
    /// else in the type system says so.
    #[test]
    fn a_search_row_keeps_name_one_address_and_a_km_distance_drops_the_rest() {
        let feature = rows(
            r#"{"properties":{
                "name":"Starbucks",
                "mapbox_id":"opaque-id-nobody-reads",
                "feature_type":"poi",
                "maki":"cafe",
                "language":"en",
                "address":"15885 Dam Road",
                "full_address":"15885 Dam Road, Clearlake, California 95422, United States",
                "poi_category":["cafe","coffee"],
                "distance":19568
            }}"#,
        );
        let row = search_result_row(&feature);

        assert_eq!(row["name"], "Starbucks");
        // `full_address` wins over the bare `address` it contains.
        assert_eq!(
            row["address"],
            "15885 Dam Road, Clearlake, California 95422, United States"
        );
        assert_eq!(row["distance"], "19.6 km");
        // `poi_category`'s entries, joined — not `feature_type`, which only
        // stands in when a result has no category at all.
        assert_eq!(row["category"], "cafe, coffee");
        for dropped in ["mapbox_id", "feature_type", "maki", "language"] {
            assert!(row.get(dropped).is_none(), "kept {dropped}: {row}");
        }
    }

    /// `full_address` is the common case; `place_formatted` and bare
    /// `address` are what a result missing it might have instead.
    #[test]
    fn a_search_row_falls_back_through_the_address_fields_it_has() {
        let with_place = rows(r#"{"properties":{"name":"a","place_formatted":"Clearlake, CA"}}"#);
        assert_eq!(search_result_row(&with_place)["address"], "Clearlake, CA");

        let with_address_only = rows(r#"{"properties":{"name":"a","address":"15885 Dam Road"}}"#);
        assert_eq!(
            search_result_row(&with_address_only)["address"],
            "15885 Dam Road"
        );

        // Both stand-ins present and no `full_address`: the street line wins,
        // being the half that says which building rather than which city.
        let with_both = rows(
            r#"{"properties":{"name":"a","address":"15885 Dam Road","place_formatted":"Clearlake, CA"}}"#,
        );
        assert_eq!(search_result_row(&with_both)["address"], "15885 Dam Road");

        // `full_address` outranks both, since it is already the two joined.
        let with_all = rows(
            r#"{"properties":{"name":"a","full_address":"15885 Dam Road, Clearlake, CA","address":"15885 Dam Road","place_formatted":"Clearlake, CA"}}"#,
        );
        assert_eq!(
            search_result_row(&with_all)["address"],
            "15885 Dam Road, Clearlake, CA"
        );

        let with_none = rows(r#"{"properties":{"name":"a"}}"#);
        assert!(search_result_row(&with_none).get("address").is_none());
    }

    /// `feature_type` only stands in for `category` when there is no
    /// `poi_category` — an address result, say, which has neither.
    #[test]
    fn a_search_row_falls_back_to_feature_type_with_no_poi_category() {
        let no_category = rows(r#"{"properties":{"name":"a","feature_type":"address"}}"#);
        assert_eq!(search_result_row(&no_category)["category"], "address");

        let empty_category =
            rows(r#"{"properties":{"name":"a","feature_type":"place","poi_category":[]}}"#);
        assert_eq!(search_result_row(&empty_category)["category"], "place");

        let neither = rows(r#"{"properties":{"name":"a"}}"#);
        assert!(search_result_row(&neither).get("category").is_none());
    }

    #[test]
    fn geocoder_extracts_one_row_per_feature_everything_else_does_not() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}},
                {"properties":{"name":"Tampere","feature_type":"place","full_address":"Tampere, Pirkanmaa, Finland"}}
            ]}"#,
        );

        let extracted = feature_collection_rows(&fc).expect("a FeatureCollection extracts");
        let list = render_feature_list(&extracted).text;
        assert!(
            list.contains("1. Helsinki") && list.contains("2. Tampere"),
            "{list}"
        );

        assert!(feature_collection_rows(&rows(r#"[{"name":"a"}]"#)).is_none());
        assert!(feature_collection_rows(&rows(r#"{"scopes":["a"]}"#)).is_none());
    }

    /// A shape close enough to fool the `type` check but not the rest of it
    /// must fall back whole, not panic partway through.
    #[test]
    fn a_feature_collection_that_cannot_parse_falls_back_whole() {
        let one_broken_feature = rows(
            r#"{"type":"FeatureCollection","features":[
                {"properties":{"name":"Helsinki"}},
                {"geometry":{"coordinates":[0,0]}}
            ]}"#,
        );
        assert!(feature_collection_rows(&one_broken_feature).is_none());

        let features_not_an_array = rows(r#"{"type":"FeatureCollection","features":"nope"}"#);
        assert!(feature_collection_rows(&features_not_an_array).is_none());

        let no_features_at_all = rows(r#"{"type":"FeatureCollection"}"#);
        assert!(feature_collection_rows(&no_features_at_all).is_none());
    }

    /// An empty row is the same failure as a missing `properties` object, one
    /// step later: the feature was there and nothing in it was recognized.
    /// Both have to fall the whole collection back to JSON — a numbered
    /// `(unnamed)` with no lines under it reads as a result that is genuinely
    /// blank rather than as a shape this renderer does not understand.
    #[test]
    fn a_feature_with_no_recognised_properties_falls_back_to_json() {
        for properties in ["{}", r#"{"mapbox_id":"opaque-id-nobody-reads"}"#] {
            let fc = rows(&format!(
                r#"{{"type":"FeatureCollection","features":[{{"properties":{properties}}}]}}"#
            ));
            assert!(
                feature_collection_rows(&fc).is_none(),
                "properties {properties} should fall back"
            );
        }

        // Tilequery is mostly shielded from this by `extra`, but a feature
        // whose properties are all nested still lands there.
        let queried = rows(
            r#"{"type":"FeatureCollection","features":[{"properties":{"context":{"tile":{"z":16}}}}]}"#,
        );
        assert!(tilequery_feature_rows(&queried).is_none());
    }

    /// No results is an answer, not a failure — `(none)` says so, where a
    /// fallback to `{"features": []}` would make the reader parse JSON to
    /// learn nothing was found.
    #[test]
    fn an_empty_feature_collection_renders_as_none_for_both_services() {
        let empty = rows(r#"{"type":"FeatureCollection","features":[]}"#);

        for rendered in [
            list_rendering(&empty, Some("geocoder")),
            list_rendering(&empty, Some("tilequery")),
        ] {
            assert_eq!(rendered.expect("a list").text, "(none)");
        }
    }

    /// The list is three services' exception, not a shape-sniffing rule:
    /// every other service's GeoJSON still has to reach pretty JSON, which is
    /// what the `None`s here mean.
    #[test]
    fn only_the_three_listed_services_turn_a_feature_collection_into_a_list() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[24.94,60.16]},"properties":{"name":"Helsinki","feature_type":"place"}}
            ]}"#,
        );

        for service in [Some("maps"), Some("styles"), Some("geocoding"), None] {
            assert!(
                list_rendering(&fc, service).is_none(),
                "{service:?} should not get a list"
            );
        }
        assert!(render_human(&fc).is_none(), "and nothing else renders it");

        for service in ["search", "geocoder", "tilequery"] {
            assert!(
                list_rendering(&fc, Some(service)).is_some(),
                "{service} should get a list"
            );
        }
    }

    #[test]
    fn a_feature_list_never_clips_an_address() {
        let long_address =
            "1600 Pennsylvania Avenue Northwest, Washington, District of Columbia 20500, United States";
        let fc = rows(&format!(
            r#"{{"type":"FeatureCollection","features":[
                {{"properties":{{"name":"The White House","feature_type":"poi","full_address":"{long_address}"}}}}
            ]}}"#
        ));

        let extracted = feature_collection_rows(&fc).unwrap();
        let list = render_feature_list(&extracted).text;

        assert_eq!(list, format!("1. The White House (poi)\n   {long_address}"));
    }

    /// Pins docs/commands.md's `forward-geocode` example to the real render,
    /// attribution line and all.
    #[test]
    fn geocoder_outputs_example_in_the_docs_matches_the_real_render() {
        let forward = rows(
            r#"{"type":"FeatureCollection","attribution":"NOTICE: © 2026 Mapbox and its suppliers. All rights reserved. This response and the information it contains may not be retained.","features":[{"type":"Feature","geometry":{"coordinates":[24.941822,60.167507],"type":"Point"},"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}}]}"#,
        );
        assert_eq!(
            render_geocoder_list(&forward).expect("a list").text,
            "1. Helsinki (place)\n   Helsinki, Uusimaa, Finland\n   24.941822,60.167507\n\n\
             NOTICE: © 2026 Mapbox and its suppliers. All rights reserved. This response and \
             the information it contains may not be retained."
        );
    }

    /// The terms the results come under are part of the answer. Unlike the
    /// `batch` guard, an unexpected key here is carried, not a reason to bail.
    #[test]
    fn attribution_follows_the_list_when_the_response_carries_one() {
        let notice = "NOTICE: © 2026 Mapbox and its suppliers. All rights reserved.";
        let with = rows(&format!(
            r#"{{"type":"FeatureCollection","attribution":"{notice}",
                "features":[{{"properties":{{"name":"Helsinki","feature_type":"place"}}}}]}}"#
        ));
        assert_eq!(
            render_geocoder_list(&with).expect("a list").text,
            format!("1. Helsinki (place)\n\n{notice}")
        );

        let without = rows(
            r#"{"type":"FeatureCollection","features":[{"properties":{"name":"Helsinki","feature_type":"place"}}]}"#,
        );
        assert_eq!(
            render_geocoder_list(&without).expect("a list").text,
            "1. Helsinki (place)"
        );

        // A blank one says nothing and would just add two empty lines.
        let blank = rows(
            r#"{"type":"FeatureCollection","attribution":"  ","features":[{"properties":{"name":"Helsinki"}}]}"#,
        );
        assert_eq!(
            render_geocoder_list(&blank).expect("a list").text,
            "1. Helsinki"
        );
    }

    /// Geocoding v6 requires `attribution` on every entry of a batch, and it
    /// is the same notice each time — fifty copies of it would bury the fifty
    /// results. One copy, under the whole batch. Two that genuinely differ
    /// both survive, since neither can be assumed to cover the other.
    #[test]
    fn a_batchs_repeated_attribution_is_printed_once_under_the_whole_batch() {
        let repeated = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","attribution":"NOTICE: terms","features":[{"properties":{"name":"Helsinki"}}]},
                {"type":"FeatureCollection","attribution":"NOTICE: terms","features":[{"properties":{"name":"Tampere"}}]}
            ]}"#,
        );
        assert_eq!(
            render_batch_feature_list(&repeated).expect("a batch").text,
            "Query 1:\n1. Helsinki\n\nQuery 2:\n1. Tampere\n\nNOTICE: terms"
        );

        let differing = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","attribution":"NOTICE: first","features":[{"properties":{"name":"Helsinki"}}]},
                {"type":"FeatureCollection","attribution":"NOTICE: second","features":[{"properties":{"name":"Tampere"}}]}
            ]}"#,
        );
        assert_eq!(
            render_batch_feature_list(&differing).expect("a batch").text,
            "Query 1:\n1. Helsinki\n\nQuery 2:\n1. Tampere\n\nNOTICE: first\n\nNOTICE: second"
        );
    }

    /// `longitude,latitude`, from the geometry, not `properties.coordinates`
    /// (`latitude` first there).
    #[test]
    fn a_feature_row_reads_coordinates_from_the_geometry_not_properties() {
        let feature = rows(
            r#"{
                "geometry": {"type": "Point", "coordinates": [24.941822, 60.167507]},
                "properties": {
                    "name": "a",
                    "coordinates": {"longitude": 60.167507, "latitude": 24.941822}
                }
            }"#,
        );
        assert_eq!(feature_row(&feature)["coordinates"], "24.941822,60.167507");

        let no_geometry = rows(r#"{"properties":{"name":"a"}}"#);
        assert!(feature_row(&no_geometry).get("coordinates").is_none());
    }

    /// A tileset may carry a top-level attribute with the same name as one of
    /// `tilequery`'s own — `geometry` is well within `mapbox-streets-v8`
    /// range. Before the dotted keys, the nested one overwrote the top-level
    /// one on its way into `extra`, and neither the value nor the fact that
    /// anything had been lost showed up anywhere.
    #[test]
    fn a_tilequery_key_does_not_overwrite_a_top_level_one_of_the_same_name() {
        let feature = rows(
            r#"{"geometry":{"coordinates":[0,0]},"properties":{
                "type":"building",
                "geometry":"multipolygon",
                "zoom":14,
                "tilequery":{"layer":"building","distance":0,"geometry":"polygon","zoom":16}
            }}"#,
        );

        let row = tilequery_row(&feature);
        assert_eq!(row["extra"]["geometry"], "multipolygon");
        assert_eq!(row["extra"]["tilequery.geometry"], "polygon");
        assert_eq!(row["extra"]["zoom"], 14);
        assert_eq!(row["extra"]["tilequery.zoom"], 16);
        assert_eq!(
            render_feature_list(&[row]).text,
            "1. building (building) — 0.0 m\n   0,0\n   geometry: multipolygon\n   \
             tilequery.geometry: polygon\n   tilequery.zoom: 16\n   zoom: 14"
        );
    }

    /// A GeoJSON position may legally carry altitude as a third element.
    /// Reading only the first two is right; refusing the whole line because
    /// there is a third is not — that dropped the coordinates entirely.
    #[test]
    fn a_three_element_position_still_produces_a_coordinate_line() {
        let geocoded = rows(
            r#"{"geometry":{"coordinates":[24.941822,60.167507,12.5]},"properties":{"name":"a"}}"#,
        );
        assert_eq!(feature_row(&geocoded)["coordinates"], "24.941822,60.167507");

        let queried = rows(
            r#"{"geometry":{"coordinates":[24.9414,60.1699,3]},"properties":{"type":"building"}}"#,
        );
        assert_eq!(tilequery_row(&queried)["coordinates"], "24.9414,60.1699");
    }

    #[test]
    fn a_feature_list_shows_coordinates_on_their_own_line() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[24.941822,60.167507]},"properties":{"name":"a","full_address":"Helsinki, Uusimaa, Finland"}}
            ]}"#,
        );
        let extracted = feature_collection_rows(&fc).unwrap();
        let list = render_feature_list(&extracted).text;
        assert_eq!(
            list,
            "1. a\n   Helsinki, Uusimaa, Finland\n   24.941822,60.167507"
        );
    }

    #[test]
    fn a_feature_row_keeps_name_one_address_and_a_category_drops_the_rest() {
        let feature = rows(
            r#"{"properties":{
                "name":"Helsinki",
                "mapbox_id":"opaque-id-nobody-reads",
                "feature_type":"place",
                "name_preferred":"Helsinki",
                "full_address":"Helsinki, Uusimaa, Finland",
                "place_formatted":"Uusimaa, Finland",
                "context":{"country":{"name":"Finland"}}
            }}"#,
        );

        let row = feature_row(&feature);
        assert_eq!(
            row,
            rows(
                r#"{"name":"Helsinki","address":"Helsinki, Uusimaa, Finland","category":"place"}"#
            )
        );
    }

    /// An explicit JSON `null` for `full_address` must fall through to
    /// `place_formatted` the same way an absent key does.
    #[test]
    fn a_null_full_address_falls_through_to_place_formatted() {
        let feature = rows(
            r#"{"properties":{
                "name":"Uusimaa",
                "feature_type":"region",
                "full_address":null,
                "place_formatted":"Finland"
            }}"#,
        );

        assert_eq!(feature_row(&feature)["address"], "Finland");
    }

    /// `tilequery_row`'s rule, applied to the two fields `feature_row` was
    /// still cloning in raw: a wrong-typed value is dropped, never carried
    /// into the row for `render_feature_list` to swallow without a word.
    #[test]
    fn a_feature_row_drops_a_non_string_name_and_address() {
        let wrong_types = rows(
            r#"{"properties":{"name":42,"full_address":{"line1":"x"},"place_formatted":["y"]}}"#,
        );
        let row = feature_row(&wrong_types);
        assert!(row.get("name").is_none(), "{row}");
        assert!(row.get("address").is_none(), "{row}");

        // And a wrong-typed `full_address` falls through to `place_formatted`
        // the same way an explicit null does.
        let one_usable = rows(r#"{"properties":{"full_address":42,"place_formatted":"Finland"}}"#);
        assert_eq!(feature_row(&one_usable)["address"], "Finland");

        // An empty `name` is absent, the same as in `tilequery_row` — kept it
        // would render as `1. ` with a trailing space.
        let blank_name = rows(r#"{"properties":{"name":"","feature_type":"place"}}"#);
        let row = feature_row(&blank_name);
        assert!(row.get("name").is_none(), "{row}");
        assert_eq!(render_feature_list(&[row]).text, "1. (unnamed) (place)");
    }

    #[test]
    fn a_batch_renders_each_querys_list_under_its_own_numbered_header() {
        let batch = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","features":[{"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}}]},
                {"type":"FeatureCollection","features":[{"properties":{"name":"Tampere","feature_type":"place","full_address":"Tampere, Pirkanmaa, Finland"}}]}
            ]}"#,
        );

        assert_eq!(
            render_batch_feature_list(&batch)
                .expect("a batch renders")
                .text,
            "Query 1:\n1. Helsinki (place)\n   Helsinki, Uusimaa, Finland\n\n\
             Query 2:\n1. Tampere (place)\n   Tampere, Pirkanmaa, Finland"
        );
    }

    /// `Query 1:` over the only query on screen names something the reader
    /// has nothing to tell apart from. Two or more keep their headers —
    /// `a_batch_renders_each_querys_list_under_its_own_numbered_header`
    /// pins that side.
    #[test]
    fn a_single_query_batch_renders_without_a_header() {
        let one = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","features":[{"properties":{"name":"Helsinki","feature_type":"place","full_address":"Helsinki, Uusimaa, Finland"}}]}
            ]}"#,
        );

        assert_eq!(
            render_batch_feature_list(&one).expect("a batch").text,
            "1. Helsinki (place)\n   Helsinki, Uusimaa, Finland"
        );
    }

    #[test]
    fn a_batch_with_one_malformed_query_falls_back_whole() {
        let batch = rows(
            r#"{"batch":[
                {"type":"FeatureCollection","features":[{"properties":{"name":"Helsinki"}}]},
                {"type":"FeatureCollection","features":[{"geometry":{"coordinates":[0,0]}}]}
            ]}"#,
        );
        assert!(render_batch_feature_list(&batch).is_none());

        assert!(render_batch_feature_list(&rows(r#"{"batch":"nope"}"#)).is_none());
        assert!(render_batch_feature_list(&rows(r#"[1,2,3]"#)).is_none());
        // A second key alongside `batch` would be lost by rendering `batch`
        // alone, so this must fall back too, not just anything with a
        // `batch` array.
        assert!(render_batch_feature_list(&rows(
            r#"{"batch":[{"type":"FeatureCollection","features":[]}],"other":1}"#
        ))
        .is_none());
    }

    /// A `building` feature has no `name` property at all — falls through
    /// to `type`, `layer` and `distance` as before, with the tile attributes
    /// that have no named slot (`height`, and `tilequery.geometry`) carried
    /// under `extra` instead of dropped.
    #[test]
    fn tilequery_row_falls_back_to_type_layer_and_distance_without_a_name() {
        let feature = rows(
            r#"{
                "geometry": {"type": "Point", "coordinates": [24.9414, 60.1699]},
                "properties": {
                    "type": "building:part",
                    "height": 9.3,
                    "tilequery": {"layer": "building", "distance": 0.855, "geometry": "polygon"}
                }
            }"#,
        );

        assert_eq!(
            tilequery_row(&feature),
            rows(
                r#"{"name":"building:part","coordinates":"24.9414,60.1699","category":"building","distance":"0.9 m","extra":{"height":9.3,"tilequery.geometry":"polygon"}}"#
            )
        );

        assert!(tilequery_row(&rows(r#"{"geometry":{}}"#))
            .as_object()
            .is_none());
    }

    /// A raster-array tileset sends no `type` to stand in for a name and no
    /// `distance`, and puts the answer the caller asked for in `val` — one
    /// number per band, so a *list*, which a scalars-only rule would have
    /// dropped along with the point of the query. The fixture is the
    /// tilequery spec's own `rasterarray` response example.
    #[test]
    fn a_raster_array_tilequery_row_keeps_its_value_band_zoom_and_units() {
        let feature = rows(
            r#"{
                "type": "Feature",
                "id": null,
                "geometry": {"type": "Point", "coordinates": [-122.459, 37.7754]},
                "properties": {
                    "val": [1.23],
                    "tilequery": {"layer": "data", "band": "frame-0", "zoom": 6, "units": "m"}
                }
            }"#,
        );

        assert_eq!(
            tilequery_row(&feature),
            rows(
                r#"{"coordinates":"-122.459,37.7754","category":"data",
                    "extra":{"tilequery.band":"frame-0","tilequery.units":"m",
                             "tilequery.zoom":6,"val":[1.23]}}"#
            )
        );

        // Several bands, several numbers, all of them on the one line.
        let three_bands =
            rows(r#"{"properties":{"val":[1.23,4.5,6.78],"tilequery":{"layer":"data"}}}"#);
        assert_eq!(
            render_feature_list(&[tilequery_row(&three_bands)]).text,
            "1. (unnamed) (data)\n   val: 1.23, 4.5, 6.78"
        );
    }

    /// An object, or a list of them, has no honest `key: value` line and is
    /// skipped rather than stringified — but skipping it must not take the
    /// values beside it, scalar or flat list, with it.
    #[test]
    fn tilequery_extras_keep_scalars_and_flat_lists_and_skip_what_is_deeper() {
        let feature = rows(
            r#"{"geometry":{"coordinates":[0,0]},"properties":{
                "type":"building",
                "class":"residential",
                "underground":false,
                "bands":["a","b"],
                "context":{"tile":{"z":16}},
                "outline":[[0,0],[1,1]],
                "tilequery":{"layer":"building","distance":0,"geometry":"polygon"}
            }}"#,
        );

        let row = tilequery_row(&feature);
        let extra = row["extra"].as_object().expect("extras");
        assert_eq!(
            extra.keys().collect::<Vec<_>>(),
            ["bands", "class", "tilequery.geometry", "underground"]
        );
        assert_eq!(
            render_feature_list(&[row]).text,
            "1. building (building) — 0.0 m\n   0,0\n   bands: a, b\n   class: residential\n   \
             tilequery.geometry: polygon\n   underground: no"
        );
    }

    /// Pins docs/commands.md's two `get-tilequery` examples to the real
    /// render — the vector one, whose `height` has to reach the text column
    /// now that it reaches the JSON one, and the raster-array one, which has
    /// no name to show and carries its sample under `val`.
    #[test]
    fn tilequery_outputs_examples_in_the_docs_match_the_real_render() {
        let vector = rows(
            r#"{"features":[{"geometry":{"coordinates":[24.94,60.16],"type":"Point"},"properties":{"height":6.2,"tilequery":{"distance":0,"layer":"building"},"type":"building"},"type":"Feature"}],"type":"FeatureCollection"}"#,
        );
        assert_eq!(
            render_feature_list(&tilequery_feature_rows(&vector).expect("a list")).text,
            "1. building (building) — 0.0 m\n   24.94,60.16\n   height: 6.2"
        );

        let raster_array = rows(
            r#"{"features":[{"geometry":{"coordinates":[-122.459,37.7754],"type":"Point"},"id":null,"properties":{"tilequery":{"band":"frame-0","layer":"data","units":"m","zoom":6},"val":[1.23]},"type":"Feature"}],"type":"FeatureCollection"}"#,
        );
        assert_eq!(
            render_feature_list(&tilequery_feature_rows(&raster_array).expect("a list")).text,
            "1. (unnamed) (data)\n   -122.459,37.7754\n   tilequery.band: frame-0\n   \
             tilequery.units: m\n   tilequery.zoom: 6\n   val: 1.23"
        );
    }

    /// A `poi_label` feature carries both `name` (the place) and `type`
    /// (its category) — `name` wins the label, matching what a human expects
    /// to read, and `type` then has to survive as an attribute. Holding it
    /// back because it *might* have been the label lost "Restaurant"
    /// entirely, which is half of what a POI result says.
    ///
    /// Asserted as a whole row, not just `name`: checking one field is what
    /// let that loss through in the first place.
    #[test]
    fn tilequery_row_prefers_name_over_type_and_keeps_type_as_an_attribute() {
        let feature = rows(
            r#"{
                "geometry": {"coordinates": [24.9414, 60.1699]},
                "properties": {
                    "name": "Bangkok9",
                    "type": "Restaurant",
                    "tilequery": {"layer": "poi_label", "distance": 4.65, "geometry": "point"}
                }
            }"#,
        );

        let row = tilequery_row(&feature);
        assert_eq!(
            row,
            rows(
                r#"{"name":"Bangkok9","coordinates":"24.9414,60.1699","category":"poi_label",
                    "distance":"4.7 m",
                    "extra":{"tilequery.geometry":"point","type":"Restaurant"}}"#
            )
        );
        assert_eq!(row["extra"]["type"], "Restaurant");
    }

    /// An empty `name` string is treated as absent, not as a blank line —
    /// and not re-emitted under `extra` either, where it would render as a
    /// bare `name:` with nothing after it. It was read, and it said nothing.
    #[test]
    fn tilequery_row_falls_back_to_type_when_name_is_an_empty_string() {
        let feature =
            rows(r#"{"geometry":{"coordinates":[0,0]},"properties":{"name":"","type":"suburb"}}"#);
        assert_eq!(
            tilequery_row(&feature),
            rows(r#"{"name":"suburb","coordinates":"0,0"}"#)
        );
    }

    /// Not every tileset is `mapbox-streets-v8`; a `type` attribute that
    /// isn't a string must never become the `name` for `render_feature_list`
    /// to choke on with `unwrap_or("(unnamed)")` silently hiding it. It is
    /// still an attribute the tileset sent, though, so it stays under
    /// `extra` rather than vanishing.
    #[test]
    fn tilequery_row_keeps_a_non_string_type_as_an_attribute_never_as_the_name() {
        let feature = rows(r#"{"geometry":{"coordinates":[0,0]},"properties":{"type":42}}"#);
        assert_eq!(
            tilequery_row(&feature),
            rows(r#"{"coordinates":"0,0","extra":{"type":42}}"#)
        );
    }

    /// Same for `name`: a non-string `name` never becomes the label and
    /// `type` stands in, while the value itself stays visible under `extra`.
    ///
    /// Asserted as a whole row, so that what becomes of the value it could
    /// not use is pinned rather than left to chance.
    #[test]
    fn tilequery_row_drops_a_non_string_name_and_falls_back_to_type() {
        let feature =
            rows(r#"{"geometry":{"coordinates":[0,0]},"properties":{"name":42,"type":"road"}}"#);
        assert_eq!(
            tilequery_row(&feature),
            rows(r#"{"name":"road","coordinates":"0,0","extra":{"name":42}}"#)
        );
    }

    #[test]
    fn tilequery_extracts_one_row_per_feature_with_distance_on_the_first_line() {
        let fc = rows(
            r#"{"type":"FeatureCollection","features":[
                {"geometry":{"coordinates":[24.9414,60.1699]},"properties":{"type":"retail","tilequery":{"layer":"building","distance":0}}}
            ]}"#,
        );
        let extracted = tilequery_feature_rows(&fc).expect("a FeatureCollection extracts");
        assert_eq!(
            render_feature_list(&extracted).text,
            "1. retail (building) — 0.0 m\n   24.9414,60.1699"
        );

        assert!(tilequery_feature_rows(&rows(r#"[{"type":"retail"}]"#)).is_none());
    }

    #[test]
    fn auto_follows_the_terminal() {
        assert_eq!(Mode::resolve(AUTO, true), Mode::Text);
        assert_eq!(Mode::resolve(AUTO, false), Mode::Json { pretty: false });
    }

    #[test]
    fn an_explicit_value_ignores_the_terminal() {
        for is_tty in [true, false] {
            assert_eq!(Mode::resolve(JSON, is_tty), Mode::Json { pretty: is_tty });
            assert_eq!(Mode::resolve(TEXT, is_tty), Mode::Text);
        }
    }

    /// `-o json` is asked for by a person as often as by a program, and the
    /// two want different whitespace out of the same document.
    #[test]
    fn json_is_indented_at_a_terminal_and_one_line_in_a_pipe() {
        assert_eq!(Mode::resolve(JSON, true), Mode::Json { pretty: true });
        assert_eq!(Mode::resolve(JSON, false), Mode::Json { pretty: false });
        // `auto` only ever reaches JSON by way of a pipe, so never indented.
        assert_eq!(Mode::resolve(AUTO, false), Mode::Json { pretty: false });
    }

    #[test]
    fn an_unrecognised_value_falls_back_to_auto() {
        assert_eq!(Mode::resolve("", true), Mode::Text);
        assert_eq!(Mode::resolve("yaml", false), Mode::Json { pretty: false });
    }

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn every_spelling_clap_accepts_is_recognised_on_argv() {
        for line in [
            &["mapbox", "--output", "json", "styles", "list"][..],
            &["mapbox", "--output=json", "styles", "list"][..],
            &["mapbox", "-o", "json", "styles", "list"][..],
            &["mapbox", "-ojson", "styles", "list"][..],
            &["mapbox", "-o=json", "styles", "list"][..],
            &["mapbox", "styles", "list", "-o", "json"][..],
        ] {
            assert_eq!(
                requested_in_argv(&argv(line)).as_deref(),
                Some("json"),
                "{line:?}"
            );
        }
    }

    #[test]
    fn a_line_without_the_flag_requests_nothing() {
        assert_eq!(
            requested_in_argv(&argv(&["mapbox", "styles", "list"])),
            None
        );
        // A bare `-o` with nothing after it, and a value that is another flag.
        assert_eq!(requested_in_argv(&argv(&["mapbox", "-o"])), None);
        assert_eq!(
            requested_in_argv(&argv(&["mapbox", "-o", "--debug"])).as_deref(),
            Some("--debug")
        );
    }

    #[test]
    fn nothing_past_a_double_dash_is_ours() {
        assert_eq!(
            requested_in_argv(&argv(&["mapbox", "tilesets-cli", "--", "-o", "json"])),
            None
        );
    }

    #[test]
    fn http_errors_take_their_message_from_the_body() {
        let e = CliError::http(401, r#"{"message":"Not Authorized - Invalid Token"}"#);
        assert_eq!(e.code, "http_401");
        assert_eq!(e.message, "Not Authorized - Invalid Token");
        assert_eq!(e.status, Some(401));
        assert!(e.body.is_some());
    }

    #[test]
    fn a_non_json_body_becomes_the_message_itself() {
        let e = CliError::http(404, "Not found\n");
        assert_eq!(e.message, "Not found");
        assert!(e.body.is_none());
    }

    #[test]
    fn a_bare_json_string_body_keeps_its_text() {
        // What the Tilesets API answers a wrong-account token with.
        let e = CliError::http(404, r#""Not found""#);
        assert_eq!(e.message, "Not found");
    }

    #[test]
    fn a_blank_message_field_does_not_become_the_message() {
        for body in [r#"{"message":""}"#, r#"{"message":"   "}"#] {
            assert_eq!(
                CliError::http(403, body).message,
                "Request failed with HTTP 403",
                "{body}"
            );
        }
    }

    #[test]
    fn a_null_body_is_treated_as_no_body() {
        let e = CliError::http(500, "null");
        assert!(e.body.is_none(), "a literal null would print as `null`");
        assert_eq!(e.message, "Request failed with HTTP 500");
    }

    #[test]
    fn an_html_error_page_is_capped_but_kept_in_full() {
        let page = format!("<html><body>{}</body></html>", "x".repeat(5_000));
        let e = CliError::http(502, &page);

        assert!(
            e.message.chars().count() <= 201,
            "message ran to {} chars",
            e.message.chars().count()
        );
        assert!(e.message.ends_with('…'));
        assert_eq!(e.body_text.as_deref(), Some(page.as_str()));
    }

    #[test]
    fn only_the_first_line_of_a_text_body_becomes_the_message() {
        let e = CliError::http(
            500,
            "Internal Server Error
request-id: abc123
",
        );
        assert_eq!(e.message, "Internal Server Error");
        assert!(e.body_text.as_deref().is_some_and(|t| t.contains("abc123")));
    }

    #[test]
    fn an_empty_or_messageless_body_falls_back_to_the_status() {
        assert_eq!(
            CliError::http(500, "").message,
            "Request failed with HTTP 500"
        );
        assert_eq!(
            CliError::http(500, r#"{"detail":"boom"}"#).message,
            "Request failed with HTTP 500"
        );
    }

    #[test]
    fn a_body_that_only_repeats_the_message_is_dropped_from_both_renderings() {
        let bare = CliError::http(401, r#"{"message":"Not Authorized - No Token"}"#);
        assert_eq!(bare.message, "Not Authorized - No Token");
        assert!(!adds_detail(bare.body.as_ref().expect("parsed")));

        let detailed = CliError::http(401, r#"{"error_code":"INVALID_TOKEN","message":"nope"}"#);
        assert!(adds_detail(detailed.body.as_ref().expect("parsed")));
    }

    #[test]
    fn only_a_body_with_more_than_a_message_is_worth_printing() {
        assert!(!adds_detail(&json!({ "message": "nope" })));
        assert!(adds_detail(&json!({ "message": "nope", "code": 12 })));
        assert!(adds_detail(&json!(["a"])));
    }

    /// The `json` rendering carries the request id on every failure that had
    /// one. A consumer logging errors wants it on all of them, and a field
    /// costs nothing to ignore.
    #[test]
    fn the_json_error_carries_the_request_id_at_any_status() {
        for status in [404u16, 429, 500, 503] {
            let err = CliError::http(status, r#"{"message":"nope"}"#)
                .with_request_id(Some("req-abc123".to_string()));
            let payload = error_payload(&err);
            assert_eq!(
                payload["request_id"],
                json!("req-abc123"),
                "missing at {status}"
            );
        }
    }

    /// Absent rather than null, the same rule the other optional keys follow.
    #[test]
    fn a_failure_without_a_request_id_has_no_such_key() {
        let payload = error_payload(&CliError::http(404, r#"{"message":"nope"}"#));
        assert!(payload.get("request_id").is_none(), "{payload}");
    }

    /// The `text` rendering shows it for a server fault and nothing else:
    /// a 404 on a mistyped id is the reader's to fix, and an id under it
    /// would be noise on the common case.
    #[test]
    fn the_text_error_shows_the_request_id_only_for_a_server_fault() {
        let with_id = |status: u16| {
            CliError::http(status, r#"{"message":"nope"}"#)
                .with_request_id(Some("req-abc123".to_string()))
        };

        for quiet in [400u16, 401, 403, 404, 422, 429] {
            assert_eq!(with_id(quiet).support_request_id(), None, "at {quiet}");
        }
        for loud in [500u16, 502, 503, 504] {
            assert_eq!(
                with_id(loud).support_request_id(),
                Some("req-abc123"),
                "at {loud}"
            );
        }
    }

    /// A failure with no HTTP status at all — a local one, like an unreadable
    /// `--file` — cannot have come with a request id, and must not claim one.
    #[test]
    fn a_local_failure_shows_no_request_id() {
        let err = CliError::new("invalid_file", "no such file")
            .with_request_id(Some("req-abc123".to_string()));
        assert_eq!(err.status, None);
        assert_eq!(err.support_request_id(), None);
    }
}
