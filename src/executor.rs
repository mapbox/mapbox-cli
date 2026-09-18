use std::borrow::Cow;
use std::time::Duration;

use anyhow::{anyhow, Result};
use clap::ArgMatches;

use crate::confirm;
use crate::http;
use crate::link;
use crate::output::{self, CliError, Mode};
use crate::remedy::{self, Remedy};
use crate::spec::{Operation, Parameter, RequestBody, ACCOUNT_PLACEHOLDERS, MULTIPART};

/// The query parameter the access token travels in, and what stands in for
/// it anywhere the URL is shown. Named once because getting this wrong
/// leaks a live token — `--debug` and the dry-run plan both render the
/// same URL.
const ACCESS_TOKEN: &str = "access_token";
const REDACTED: &str = "<redacted>";

/// The media type a `--data` body is sent as, unless the spec named a text one.
const JSON_CONTENT_TYPE: &str = "application/json";

/// Name of the flag that stops short of sending. Declared per operation
/// rather than globally: a `GET` has nothing to preview, and offering a
/// flag that means nothing is worse than requiring it after the operation
/// name. See [`dry_run_arg`].
pub const DRY_RUN_ARG: &str = "dry-run";

/// The wording for a command whose mutation is an API call.
pub const DRY_RUN_REQUEST_HELP: &str =
    "Print the request this would send, then exit without sending it";

/// The `--dry-run` flag, with the sentence that fits what the command does.
///
/// The help text is a parameter because mutating commands change different
/// things: a generated operation sends a request, while `mapbox auth
/// logout` deletes a local file and sends nothing at all. A `--help` line
/// that describes the wrong one is a small lie that makes people trust a
/// safety flag less.
pub fn dry_run_arg(help: &'static str) -> clap::Arg {
    clap::Arg::new(DRY_RUN_ARG)
        .long(DRY_RUN_ARG)
        .action(clap::ArgAction::SetTrue)
        .help(help)
}

/// Whether `--dry-run` was given, for an operation that may not have it.
///
/// `get_flag` panics on an unregistered argument, and read-only operations
/// never register this one — hence `try_get_one`.
pub fn wants_dry_run(matches: &ArgMatches) -> bool {
    matches
        .try_get_one::<bool>(DRY_RUN_ARG)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

/// The three switches that change whether and how a request goes out, as
/// opposed to what's being requested.
///
/// Grouped into a struct once there were three of them. Clippy's argument
/// limit is the visible reason; the real one is that three bare `bool`s in
/// a row at a call site is one transposition away from a silent swap —
/// mixing up `--debug` and `--dry-run` would send a request that was only
/// supposed to be described. Naming the fields makes that impossible.
#[derive(Clone, Copy)]
pub struct RunFlags {
    pub debug: bool,
    pub assume_yes: bool,
    pub dry_run: bool,
    /// The budget the caller asked for, from `crate::http::requested` —
    /// `--timeout` or `MAPBOX_TIMEOUT`. `None` doesn't mean "no timeout",
    /// it means "nobody said"; [`crate::http::budget`] then falls back to
    /// whatever default fits the request.
    pub timeout: Option<Duration>,
}

/// Runs one generated command — the request it would make, either sent or
/// described under `--dry-run`.
///
/// A thin wrapper so every way this can reject an *argument* points at
/// `--schema` from one place, not three: choosing between `--data` and
/// `--file`, a `--data` that won't parse, and re-parsing it on the
/// dry-run path (which reads the body instead of trusting it).
/// `with_schema_action` reacts to a specific error code, so everything
/// else — an HTTP status, an unreadable file — passes through untouched.
pub fn execute(
    op: &Operation,
    matches: &ArgMatches,
    token: Option<&str>,
    username: Option<&str>,
    flags: RunFlags,
    mode: Mode,
) -> Result<()> {
    dispatch(op, matches, token, username, flags, mode).map_err(|err| with_schema_action(err, op))
}

fn dispatch(
    op: &Operation,
    matches: &ArgMatches,
    token: Option<&str>,
    username: Option<&str>,
    flags: RunFlags,
    mode: Mode,
) -> Result<()> {
    let RunFlags {
        debug,
        assume_yes,
        dry_run,
        timeout,
    } = flags;
    let client = http::client_for(Some(op.service.as_str()))?;

    let mut path = op.path_template.clone();

    if let Some(u) = username {
        for placeholder in ACCOUNT_PLACEHOLDERS {
            path = path.replace(&format!("{{{placeholder}}}"), u);
        }
    }

    for param in &op.path_params {
        if let Some(val) = matches.get_one::<String>(&param.arg_name) {
            let safe = path_segment(&param.name, val)?;
            path = substitute_path_param(&path, &param.name, &safe, param.required);
        }
    }

    if path.contains('{') {
        let missing: Vec<&str> = path
            .split('{')
            .skip(1)
            .filter_map(|s| s.split('}').next())
            .collect();
        return Err(CliError::new(
            "missing_path_parameters",
            format!(
                "Missing required path parameters: {}. Use --username / MAPBOX_USERNAME for {{username}}/{{owner}}/{{account}}.",
                missing.join(", ")
            ),
        )
        // The message above names the two ways to supply an account; this
        // adds the command that checks whether a login already supplies it.
        .with_remedy(Remedy::default().with_action(Some("mapbox auth whoami".to_string())))
        .into());
    }

    let url = format!("{}{}", op.base_url, path);

    let mut query: Vec<(String, String)> = vec![];

    if let Some(t) = token {
        query.push((ACCESS_TOKEN.to_string(), t.to_string()));
    }

    for param in &op.query_params {
        if param.is_boolean {
            if matches.get_flag(&param.arg_name) {
                query.push((param.name.clone(), "true".to_string()));
            }
        } else if let Some(val) = matches.get_one::<String>(&param.arg_name) {
            query.push((param.name.clone(), val.clone()));
        }
    }

    // `--data`/`--file` are declared per-operation, so either may not exist
    // on this command at all — `get_one` would panic on an unregistered
    // argument, hence `try_get_one`. Resolved here, before `--dry-run` or
    // anything else reads it, so a `@path` that doesn't exist fails now
    // instead of after being described as a request that would then fail
    // to send.
    let data_argument = match matches.try_get_one::<String>("data").ok().flatten() {
        Some(value) => Some(resolve_data(value)?),
        None => None,
    };
    let data = data_argument
        .as_ref()
        .map(|argument| argument.body.as_ref());
    let files: Vec<&str> = matches
        .try_get_many::<String>("file")
        .ok()
        .flatten()
        .map(|values| values.map(String::as_str).collect())
        .unwrap_or_default();

    // Resolved before the dry-run branch on purpose: choosing between
    // `--data` and `--file` is a check the caller can fail, and a dry run
    // that skipped it would describe a request that could never actually
    // be sent.
    let body_source = match &op.body {
        Some(body) => Some(resolve_body_source(body, data, &files)?),
        None => None,
    };

    if dry_run {
        // Worth saying here specifically: a real call would answer a
        // missing token with an unmissable 401, but a dry run would just
        // print a plausible-looking request that could never actually
        // succeed.
        if token.is_none() {
            output::progress(
                "Note: no access token was resolved, so the real request would be unauthenticated.",
            );
        }
        return describe_request(op, mode, &url, &query, body_source.as_ref());
    }

    // Placed after the dry-run branch deliberately: a dry run sends
    // nothing, so asking "go ahead?" about something that isn't going to
    // happen would make the safety flag the thing that blocks a script.
    //
    // `url` here, not the redacted one — the access token is a query
    // parameter and `url` carries no query string at all, so it's already
    // safe to print. Never pass `redacted_url`'s output here.
    confirm::destructive_request(&op.method, &url, assume_yes)?;

    // Also after the dry-run branch, so a `[debug]` line always means a
    // request actually went out — never one that only looks like it did.
    if debug {
        eprintln!("[debug] {} {}", op.method, redacted_url(&url, &query));
    }

    let method = op.method.as_str();
    let mut req = match method {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "PATCH" => client.patch(&url),
        "DELETE" => client.delete(&url),
        _ => client.get(&url),
    }
    .query(&query)
    // Set per-request, not on the client: the client has no way to know
    // what a given request is doing, and a sprite upload (needs a long
    // timeout) and a listing (needs a short one) can't share one budget.
    // `reqwest` prefers the request's own timeout over the client's —
    // 0.12.28's `execute_request` reads
    // `req.timeout().copied().or(self.timeout.0)` — so this is what
    // actually applies.
    .timeout(http::budget(
        timeout,
        payload_of(
            body_source.as_ref(),
            data_argument
                .as_ref()
                .is_some_and(|argument| argument.streamed),
        ),
    ));

    if let Some(source) = body_source {
        req = attach_body(req, source)?;
    }

    let response = req
        .send()
        .map_err(|e| transport_failure("Request failed", e))?;
    let status = response.status();
    // Must read headers before `bytes()` consumes the response — anything
    // not taken here is gone after. For a long time only `Content-Type`
    // survived this point, which is why pagination and support escalation
    // used to be unreachable; see [`ResponseHeaders`].
    let headers = ResponseHeaders::read(response.headers());
    let content_type = headers.content_type.clone();
    let body = response
        .bytes()
        .map_err(|e| transport_failure("Failed to read response", e))?;

    // Six of the twelve services answer with bytes, not text — images,
    // vector tiles, glyph PBFs, style ZIPs. Decoding those as UTF-8 would
    // silently corrupt them (a PNG's leading 0x89 becomes EF BF BD and the
    // file no longer opens). But an error response is worth reading no
    // matter what the endpoint normally returns, so failures always take
    // the text path.
    let as_text = if status.is_success() && is_binary_content_type(&content_type) {
        None
    } else {
        Some(String::from_utf8_lossy(&body))
    };

    // A failure is an error, not a result, so it belongs on stderr — never
    // in a redirected file alongside real output. The body goes into the
    // error object instead of being printed here, keeping every detail the
    // old approach (dumping to stdout) showed.
    if !status.is_success() {
        let text = as_text
            .as_deref()
            .expect("a failure always takes the text path");
        return Err(CliError::http(status.as_u16(), text)
            .with_request_id(headers.request_id)
            .with_remedy(remedy::for_http(status.as_u16(), op, matches, username))
            .into());
    }

    // A 204, or a 200 with nothing in it. `serde_json` can't parse an empty
    // string, so this used to fall through to the text-body path and print
    // a blank line (or `""` under `json` — a valid document that says
    // nothing). Every no-body mutation ended up there, which made a
    // successful delete look the same as a command that never ran.
    if as_text
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
    {
        return output::emit(
            mode,
            &empty_success_text(op, matches),
            serde_json::json!({
                "ok": true,
                "status": status.as_u16(),
                "command": op.command(),
            }),
        );
    }

    // `Some` only for a listing that's one page of several.
    let next_page = headers
        .next_page
        .as_deref()
        .map(|next| NextPage::of(&op.query_params, next));

    match as_text {
        Some(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(json) => match matches.get_one::<String>(output::FILTER_ARG) {
                Some(wanted) => output::emit_value(
                    mode,
                    &output::pick_row(&json, wanted)
                        .map_err(|err| with_page_context(err, next_page.as_ref()))?,
                    None,
                    Some(&op.service),
                    // The wanted row is already in hand, so no need for a
                    // pagination tip about the other rows.
                    None,
                )?,
                None => output::emit_value(
                    mode,
                    &json,
                    detail_hint(op).as_deref(),
                    Some(&op.service),
                    next_page.as_ref().map(NextPage::tip).as_deref(),
                )?,
            },
            Err(_) => output::emit_text_body(mode, &text)?,
        },
        // Bytes bypass the output contract entirely. A PNG can't be
        // wrapped in a JSON envelope without destroying it, and
        // `--output json` on a tile endpoint is far more likely to be a
        // global flag riding along than a deliberate request to mangle
        // the image.
        None => write_binary(&body, &content_type)?,
    }

    Ok(())
}

/// The headers a Mapbox response may identify itself with, checked in this
/// order.
///
/// There are two because we measured, not assumed. `x-request-id` is the
/// conventional name and what we checked first — but no Mapbox endpoint
/// reachable from here actually sends it: styles, tokens, fonts, and
/// geocoding v6 all answer without one, success or failure. What they all
/// carry instead is `x-amz-cf-id`, the CloudFront request id, since the
/// whole API sits behind CloudFront — and that's the id support uses to
/// trace a request.
///
/// `x-request-id` stays first anyway: a service that does send one is more
/// specific than the CDN in front of it, and checking costs only a lookup
/// in a map already in memory.
const REQUEST_ID_HEADERS: [&str; 2] = ["x-request-id", "x-amz-cf-id"];

/// The request id from a response, for a caller that reads the body itself.
///
/// `text()` and `bytes()` both consume the response, so this must be
/// called before reading the body — which is why it's a named function
/// instead of being inlined at each of the three call sites.
///
/// For Mapbox-bound requests only. `agent_skills` talks to GitHub
/// codeload, which uses `x-github-request-id` instead — something Mapbox
/// support can't look up — so it deliberately doesn't call this.
pub fn request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    REQUEST_ID_HEADERS.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(String::from)
    })
}

/// What a response says about itself, past its body.
///
/// A struct instead of three reads at the call site, because `bytes()`
/// consumes the response — whatever isn't taken before that is gone for
/// good. Taking only `Content-Type` used to leave paginated listings
/// truncating silently, and a 500 with nothing to quote to support.
struct ResponseHeaders {
    /// Decides whether the body is read as text or written as bytes.
    content_type: String,
    /// What support needs to find this request in their logs. Carried
    /// into the error and never printed on success, where it's just noise.
    request_id: Option<String>,
    /// The `rel="next"` target of a `Link` header, when this response is one
    /// page of several.
    next_page: Option<String>,
}

impl ResponseHeaders {
    fn read(headers: &reqwest::header::HeaderMap) -> Self {
        // A present-but-empty header says nothing; treating it as missing
        // avoids printing "Request ID:" with nothing after it.
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };

        ResponseHeaders {
            content_type: text(reqwest::header::CONTENT_TYPE.as_str())
                .unwrap_or_default()
                .to_string(),
            request_id: request_id(headers),
            next_page: text(reqwest::header::LINK.as_str())
                .and_then(link::next_url)
                .map(String::from),
        }
    }
}

/// A response that's one page of several, and how to ask for the next one.
///
/// Holds the flags, not the URL: following the `Link` target verbatim would
/// mean re-sending a URL the API built (token and all), while flags are
/// something a caller can read, edit, and run.
struct NextPage(Option<String>);

impl NextPage {
    /// Derives the flags from the operation instead of hardcoding `--start`.
    ///
    /// The next URL's query is matched against the parameters this command
    /// declares, so the tip names whatever the spec calls its paging
    /// parameters — a service that pages differently needs no change here.
    ///
    /// **The access token can never appear in the result.** It rides in the
    /// query string of every request, so the API echoes it back in this
    /// very URL. Two safeguards, not one: `dispatch` adds the token
    /// directly rather than declaring it in `op.query_params`, so matching
    /// against declared parameters already excludes it — but a spec could
    /// declare a parameter with that same name someday, so it's also
    /// refused explicitly. `the_page_tip_never_names_the_access_token` and
    /// `a_declared_parameter_named_access_token_is_still_withheld` test
    /// both.
    fn of(declared: &[Parameter], next: &str) -> Self {
        let flags: Vec<String> = query_pairs(next)
            .into_iter()
            .filter(|(name, _)| name != ACCESS_TOKEN)
            .filter_map(|(name, value)| {
                declared
                    .iter()
                    .find(|param| param.name == name)
                    .map(|param| format!("--{} {}", param.arg_name, shell_value(&value)))
            })
            .collect();

        NextPage((!flags.is_empty()).then(|| flags.join(" ")))
    }

    /// The line printed under a listing that has more pages.
    fn tip(&self) -> String {
        match &self.0 {
            Some(flags) => format!("More results: add `{flags}` for the next page."),
            // Only reachable if the API paginates an operation whose spec
            // declares no paging parameter — a spec gap, not a user
            // error. Still worth saying, since the result is incomplete
            // either way.
            None => "More results exist, but this command declares no parameter to reach them."
                .to_string(),
        }
    }

    /// The `fix` on a `--id` that found nothing on this page.
    fn fix(&self) -> String {
        match &self.0 {
            Some(flags) => format!(
                "This is one page of results, so the id may be on a later one. \
                 Add `{flags}` to search the next page."
            ),
            None => "This is one page of results, so the id may be on a later one.".to_string(),
        }
    }
}

/// A URL's query, decoded.
///
/// Percent-decoded on purpose: these values go into a tip meant to be
/// copied onto a command line, and the CLI re-encodes whatever it's given
/// — so handing back `%2B` would round-trip to `%252B` and ask for the
/// wrong page. An unparseable URL yields nothing rather than an error; a
/// pagination tip isn't worth turning a successful request into a failure.
fn query_pairs(url: &str) -> Vec<(String, String)> {
    match reqwest::Url::parse(url) {
        Ok(parsed) => parsed
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect(),
        Err(_) => vec![],
    }
}

/// A query value as it would have to be typed into a shell.
///
/// Paging cursors are opaque ids in practice, but this tip gets pasted
/// verbatim, and an unquoted value with a space in it would silently
/// become two arguments.
fn shell_value(value: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_.~:@+,".contains(c);
    if !value.is_empty() && value.chars().all(safe) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Adds "there are more pages" to a `--id` that matched nothing.
///
/// `pick_row` says "No row has the id", which is true of this page but
/// might be false of the whole listing. On a paginated response, that's
/// the most misleading version of the truncation problem this whole path
/// exists to prevent — so the error clarifies which one it means.
fn with_page_context(err: anyhow::Error, next_page: Option<&NextPage>) -> anyhow::Error {
    let Some(next_page) = next_page else {
        return err;
    };
    match err.downcast::<CliError>() {
        // `not_a_list` is about the shape of the response — another page
        // wouldn't change that.
        Ok(cli) if cli.code == "not_found" => cli
            .with_remedy(Remedy::default().with_fix(&next_page.fix()))
            .into(),
        Ok(cli) => cli.into(),
        Err(other) => other,
    }
}

/// The request line a reader may safely see: URL, query, token replaced.
///
/// The access token rides in the query string, so every rendering of this
/// URL has to strip it. `--debug` has always done this; the dry run needs
/// it even more, since the request *is* its output — the part most likely
/// to get pasted into an issue or captured whole by CI.
fn redacted_url(url: &str, query: &[(String, String)]) -> String {
    let rendered: Vec<String> = query
        .iter()
        .map(|(name, value)| {
            let shown = shown_value(name, value);
            // The token's stand-in is spliced in literally. It is not a value
            // anyone sends, so encoding it would only turn an obvious
            // placeholder into `%3Credacted%3E`, and the URL cannot be used
            // until the reader puts their own token there regardless.
            let shown = if name == ACCESS_TOKEN {
                shown.to_string()
            } else {
                encode_query_value(shown)
            };
            format!("{}={shown}", encode_query_value(name))
        })
        .collect();

    if rendered.is_empty() {
        url.to_string()
    } else {
        format!("{url}?{}", rendered.join("&"))
    }
}

/// One query value, encoded so the rendered URL is a URL.
///
/// This existed as plain string concatenation, and the result was a line that
/// could not be used for the one thing it is printed for. A free-text query —
/// `--q "Dog friendly coffee shops near me"`, which the Search Box API now
/// takes — rendered with literal spaces, and `curl` rejects that outright:
/// the request the CLI itself made was fine, because reqwest encodes what it
/// sends, but the URL beside it on stderr was not the URL that went out and
/// could not be pasted anywhere.
///
/// `%20` rather than form-urlencoding's `+`. Both decode to a space at any
/// server that reads the query as a form, but only `%20` means a space
/// everywhere else, and `+` in a URL a person is reading is a character they
/// have to stop and think about.
///
/// The kept set is the unreserved characters of RFC 3986 plus the four
/// sub-delims and gen-delims that are legal in a query and appear in real
/// values here: `,` in a coordinate pair or a bbox, `:` and `/` in a
/// route-geometry or a style URI, `@` in a static-images overlay. Everything
/// else is escaped, which matters most for `&`, `=` and `+` — left alone they
/// change the shape of the query rather than a value in it.
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            b',' | b':' | b'/' | b'@' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// One query parameter's value, or the stand-in when it is the token.
///
/// The single place deciding what may be printed. Both renderings of the
/// query — the URL line and the JSON object — go through this, so the
/// rule can't drift between the two.
fn shown_value<'a>(name: &str, value: &'a str) -> &'a str {
    if name == ACCESS_TOKEN {
        REDACTED
    } else {
        value
    }
}

/// The dry run's answer: what the real call would send, and nothing sent.
///
/// Goes through `output::emit` and out to stdout like any other result —
/// the command was asked what it would do, and this *is* the answer, not
/// a note about it. So `-o json` gives a script an object to check before
/// letting a real delete run, and a terminal gets one readable request
/// line.
fn describe_request(
    op: &Operation,
    mode: Mode,
    url: &str,
    query: &[(String, String)],
    body: Option<&BodySource<'_>>,
) -> Result<()> {
    let method = op.method.to_ascii_uppercase();
    let described = body.map(describe_body).transpose()?.flatten();

    let mut text = format!(
        "Dry run — nothing was sent.\n{method} {}",
        redacted_url(url, query)
    );
    if let Some(described) = &described {
        text.push('\n');
        text.push_str(&described.text);
    }

    // An object, not a list of pairs: no query parameter repeats here,
    // since each comes from an argument clap keeps one value for, and
    // `.query.access_token` is more useful to a caller than
    // `.query[3].value`.
    let shown_query: serde_json::Map<String, serde_json::Value> = query
        .iter()
        .map(|(name, value)| {
            let shown = shown_value(name, value);
            (name.clone(), serde_json::Value::String(shown.to_string()))
        })
        .collect();

    output::emit(
        mode,
        &text,
        serde_json::json!({
            "dry_run": true,
            "command": op.command(),
            "method": method,
            // No query string here — `query` carries that separately.
            // Rejoining them would produce a URL nobody can use, since the
            // token in it is redacted.
            "url": url,
            "query": shown_query,
            "body": described.map(|described| described.json),
        }),
    )
}

/// A request body, as the dry run reports it.
#[derive(Debug)]
struct DescribedBody {
    text: String,
    json: serde_json::Value,
}

/// Describes the body — and validates it exactly as sending would.
///
/// Every check [`attach_body`] makes happens here too: the JSON gets
/// parsed, and each `--file` gets read, not just stat-ed. Reading matters
/// because a dry run answers "would this work?", and a file that exists
/// but can't be read would pass a metadata check and then fail the real
/// call — exactly the surprise `--dry-run` is meant to rule out. The
/// operations that take `--file` (sprites, upload chunks) are small, so
/// the read costs nothing worth skipping.
fn describe_body(source: &BodySource<'_>) -> Result<Option<DescribedBody>> {
    Ok(match source {
        BodySource::Empty => None,
        BodySource::Json(data) => {
            let json: serde_json::Value = serde_json::from_str(data).map_err(|e| {
                CliError::new("invalid_data", format!("Invalid JSON for --data: {e}"))
            })?;
            Some(DescribedBody {
                text: format!(
                    "Body: {} bytes of {JSON_CONTENT_TYPE} from --data",
                    data.len()
                ),
                json: serde_json::json!({
                    "source": "--data",
                    "content_type": JSON_CONTENT_TYPE,
                    "bytes": data.len(),
                    // The parsed document, not the raw string it was typed
                    // as — re-escaping it into a JSON string would make
                    // the one thing worth checking here unreadable.
                    "json": json,
                }),
            })
        }
        BodySource::Text { data, content_type } => Some(DescribedBody {
            text: format!("Body: {} bytes of {content_type} from --data", data.len()),
            json: serde_json::json!({
                "source": "--data",
                "content_type": content_type,
                "bytes": data.len(),
                "text": data,
            }),
        }),
        BodySource::Raw { path, content_type } => {
            let bytes = read_body_file(path)?.len();
            Some(DescribedBody {
                text: format!("Body: {path} — {bytes} bytes, sent as {content_type}"),
                json: serde_json::json!({
                    "source": "--file",
                    "content_type": content_type,
                    "bytes": bytes,
                    "files": [{ "path": path, "bytes": bytes }],
                }),
            })
        }
        BodySource::Multipart { paths, field } => {
            let mut parts = Vec::with_capacity(paths.len());
            let mut total = 0usize;
            let mut text = format!(
                "Body: {MULTIPART} — {} part{} under field `{field}`",
                paths.len(),
                if paths.len() == 1 { "" } else { "s" }
            );

            for path in paths {
                let bytes = read_body_file(path)?.len();
                let media_type = part_media_type(path);
                // Not just cosmetic: `batchUploadSprite` takes the icon
                // name from this filename, so it's also what the sprite
                // gets called.
                let name = file_name_of(path);
                total += bytes;
                text.push_str(&format!(
                    "\n  {name} (from {path}): {bytes} bytes, {media_type}"
                ));
                parts.push(serde_json::json!({
                    "path": path,
                    "name": name,
                    "bytes": bytes,
                    "content_type": media_type,
                }));
            }

            Some(DescribedBody {
                text,
                json: serde_json::json!({
                    "source": "--file",
                    "content_type": MULTIPART,
                    "field": field,
                    "bytes": total,
                    "files": parts,
                }),
            })
        }
    })
}

/// What to put in the request body, decided from the spec and the flags.
///
/// Borrows instead of owning so this decision stays free of I/O: choosing
/// what to send is separate from reading the files, and only the choosing
/// is worth testing on its own.
#[derive(Debug, PartialEq, Eq)]
enum BodySource<'a> {
    /// The operation takes a body but the caller supplied none. Still a
    /// request worth sending — the API decides whether it was required.
    Empty,
    Json(&'a str),
    /// The argument as it was typed, under the media type the spec names.
    Text {
        data: &'a str,
        content_type: &'a str,
    },
    /// One file's bytes, sent as-is under the spec's media type.
    Raw {
        path: &'a str,
        content_type: &'a str,
    },
    /// One form part per file, all under the field the spec names.
    Multipart {
        paths: Vec<&'a str>,
        field: &'a str,
    },
}

/// What `--data` was given, once a `@path` or `@-` has been read.
#[derive(Debug)]
struct DataArgument<'a> {
    /// The body itself. Borrowed when it was typed, owned when it was read.
    body: std::borrow::Cow<'a, str>,
    /// Whether it came from a file or stdin rather than argv.
    ///
    /// Carried because the timeout budget depends on it: argv caps what
    /// can be typed at roughly a megabyte, but nothing caps a file. See
    /// [`payload_of`].
    streamed: bool,
}

/// The `--data` prefix that means "read this rather than send it".
const DATA_FROM_PATH: char = '@';

/// The `@` path that means stdin, spelled as curl and every other tool spell
/// it.
const STDIN_PATH: &str = "-";

/// Resolves a `--data` argument that names a file instead of carrying a body.
///
/// `@path` reads the file, `@-` reads stdin, and anything else is the body
/// itself — the spelling curl has used long enough that people try it
/// first.
///
/// Inherits curl's ambiguity: a body whose first character is a literal
/// `@` can't be passed this way. That costs nothing here, since every
/// operation reachable with `--data` today sends JSON, and `@` isn't valid
/// JSON. If a text body that could start with `@` ever gets wired up,
/// `--data-raw` is the established escape hatch.
///
/// Read here, not at send time, so `--dry-run` validates the file too — a
/// dry run that skipped this could describe a request that would never
/// actually send.
fn resolve_data(value: &str) -> Result<DataArgument<'_>> {
    let Some(path) = value.strip_prefix(DATA_FROM_PATH) else {
        return Ok(DataArgument {
            body: std::borrow::Cow::Borrowed(value),
            streamed: false,
        });
    };

    if path.is_empty() {
        return Err(CliError::new(
            "invalid_data",
            "`--data @` names nothing to read. Use `@<path>` for a file, or `@-` for stdin.",
        )
        .into());
    }

    let (body, source) = if path == STDIN_PATH {
        (read_stdin()?, "stdin".to_string())
    } else {
        (read_data_file(path)?, format!("`{path}`"))
    };

    // An empty body would otherwise reach `attach_body` as invalid JSON —
    // "EOF while parsing a value" — describing the symptom, not the
    // mistake. The real mistake is almost always an empty pipe (`cat
    // missing.json | mapbox …`, whose own error scrolled past on the same
    // stderr), so naming the source here points at the actual cause.
    if body.trim().is_empty() {
        return Err(CliError::new(
            "invalid_data",
            format!("{source} was empty, so there is no request body to send."),
        )
        .into());
    }

    Ok(DataArgument {
        body: std::borrow::Cow::Owned(body),
        streamed: true,
    })
}

/// A `--data @path` file, as text.
///
/// Text, not bytes — that's a validity check, not just convenience. A
/// JSON body has to be UTF-8, so a file that isn't gets reported here
/// instead of silently lossy-converted into a body the API would reject
/// for reasons that don't point back at the actual mistake.
fn read_data_file(path: &str) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| {
        let message = if e.kind() == std::io::ErrorKind::InvalidData {
            format!("`{path}` is not valid UTF-8, so it cannot be sent as a JSON body.")
        } else {
            format!("Cannot read `{path}`: {e}")
        };
        CliError::new("invalid_file", message).into()
    })
}

/// A `--data @-` body, from stdin.
fn read_stdin() -> Result<String> {
    use std::io::Read;

    let mut body = String::new();
    std::io::stdin()
        .read_to_string(&mut body)
        .map_err(|e| -> anyhow::Error {
            let message = if e.kind() == std::io::ErrorKind::InvalidData {
                "stdin is not valid UTF-8, so it cannot be sent as a JSON body.".to_string()
            } else {
                format!("Cannot read the request body from stdin: {e}")
            };
            CliError::new("invalid_file", message).into()
        })?;
    Ok(body)
}

/// Picks between `--data` and `--file` for an operation that takes a body.
///
/// Kept pure: every rejection here is a mistake the caller can fix from
/// the message alone, with no request ever sent.
fn resolve_body_source<'a>(
    body: &'a RequestBody,
    data: Option<&'a str>,
    files: &[&'a str],
) -> Result<BodySource<'a>> {
    if data.is_some() && !files.is_empty() {
        return Err(CliError::new(
            "conflicting_body",
            "`--data` and `--file` both supply a request body. Pass one or the other.",
        )
        .into());
    }

    if files.is_empty() {
        return Ok(match (data, body.text_content_type()) {
            (Some(data), Some(content_type)) => BodySource::Text { data, content_type },
            (Some(data), None) => BodySource::Json(data),
            (None, _) => BodySource::Empty,
        });
    }

    let Some(content_type) = body.file_content_type() else {
        return Err(CliError::new(
            "unsupported_body",
            "This command sends a JSON body. Use `--data` rather than `--file`.",
        )
        .into());
    };

    if body.is_multipart() {
        let Some(field) = body.multipart_field.as_deref() else {
            return Err(CliError::new(
                "unsupported_body",
                "The spec declares a multipart body without naming a field to upload files under.",
            )
            .into());
        };
        return Ok(BodySource::Multipart {
            paths: files.to_vec(),
            field,
        });
    }

    // A raw body is one file by definition. clap already keeps only the
    // last `--file` for this operation, so this guards the function
    // itself, not the CLI's parsing.
    if files.len() > 1 {
        return Err(CliError::new(
            "conflicting_body",
            format!(
                "This command sends a single {content_type} body, but `--file` was given {} times.",
                files.len()
            ),
        )
        .into());
    }

    Ok(BodySource::Raw {
        path: files[0],
        content_type,
    })
}

/// How much data the request is about to send — all the timeout budget
/// depends on.
///
/// `--file` is unbounded, and so is `--data @path` or `--data @-`, which is
/// why this needs a second argument instead of just reading the body. A
/// `--data` body *typed* on the command line is capped by argv at roughly
/// a megabyte and fits the ordinary budget with room to spare — that used
/// to be true of every `--data` body, until `@path` broke it:
/// `BodySource::Json` looks identical whether it was typed or read from a
/// 200 MB file, and the second case would get a sixty-second budget it
/// could never meet.
///
/// The response isn't consulted, since nothing here has seen it yet — and
/// it wouldn't help anyway: even the six of twelve services that answer
/// with bytes (tiles, glyph ranges, style ZIPs) arrive well inside a
/// minute. Only what this CLI is *sending* is worth budgeting for.
fn payload_of(body: Option<&BodySource<'_>>, data_was_read: bool) -> http::Payload {
    match body {
        Some(BodySource::Raw { .. } | BodySource::Multipart { .. }) => http::Payload::File,
        _ if data_was_read => http::Payload::File,
        _ => http::Payload::Bounded,
    }
}

/// Reads whatever the resolved source points at and puts it on the request.
fn attach_body(
    req: reqwest::blocking::RequestBuilder,
    source: BodySource<'_>,
) -> Result<reqwest::blocking::RequestBuilder> {
    Ok(match source {
        BodySource::Empty => req,
        BodySource::Json(data) => {
            let json: serde_json::Value = serde_json::from_str(data).map_err(|e| {
                CliError::new("invalid_data", format!("Invalid JSON for --data: {e}"))
            })?;
            req.header("Content-Type", JSON_CONTENT_TYPE).json(&json)
        }
        BodySource::Text { data, content_type } => req
            .header("Content-Type", content_type)
            .body(data.to_string()),
        BodySource::Raw { path, content_type } => req
            .header("Content-Type", content_type)
            .body(read_body_file(path)?),
        BodySource::Multipart { paths, field } => {
            let mut form = reqwest::blocking::multipart::Form::new();
            for path in paths {
                let part = reqwest::blocking::multipart::Part::bytes(read_body_file(path)?)
                    .file_name(file_name_of(path).to_string())
                    .mime_str(part_media_type(path))
                    .map_err(|e| {
                        CliError::new("invalid_file", format!("Cannot send `{path}`: {e}"))
                    })?;
                form = form.part(field.to_string(), part);
            }
            req.multipart(form)
        }
    })
}

/// A file named by `--file`, as bytes.
///
/// A missing path is the most likely thing to go wrong here, and it needs
/// to read as the caller's typo, not as a crash.
fn read_body_file(path: &str) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| {
        CliError::new(
            "invalid_file",
            format!("Cannot read `{path}` given to --file: {e}"),
        )
        .into()
    })
}

/// The last path segment, which is what a multipart part is named by.
///
/// Not cosmetic for sprites: `batchUploadSprite` takes the icon name from
/// each part's filename, so `zz-clitest-1.svg` becomes the icon
/// `zz-clitest-1`.
fn file_name_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The media type to label one multipart part with.
///
/// The spec only describes these parts as `format: binary`, so the file
/// extension is the only evidence we have. Getting it wrong matters:
/// `batchUploadSprite` rejects a part that doesn't claim to be SVG.
fn part_media_type(path: &str) -> &'static str {
    let extension = file_name_of(path)
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());

    match extension.as_deref() {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// What to say when a successful response carries no body.
///
/// The HTTP method is all we have to go on: generated commands share one
/// code path, so a 204 looks the same whether a style was deleted or a
/// folder renamed. Naming the resource makes the line worth reading — the
/// last path parameter identifies it, while the earlier ones just scope it
/// (`{username}`, then `{style_id}`, then `{icon_name}`).
fn empty_success_text(op: &Operation, matches: &ArgMatches) -> String {
    let subject = op
        .path_params
        .iter()
        .filter_map(|param| matches.get_one::<String>(&param.arg_name))
        .next_back()
        .cloned();

    empty_success_line(&op.method, subject.as_deref())
}

/// The sentence itself, apart from where its two inputs came from.
fn empty_success_line(method: &str, subject: Option<&str>) -> String {
    let subject = subject.map(|s| format!(" {s}")).unwrap_or_default();
    match method.to_ascii_uppercase().as_str() {
        "DELETE" => format!("Deleted{subject}."),
        "POST" | "PUT" | "PATCH" => format!("Done{subject}."),
        // A GET answering 200 with an empty body — the session endpoints.
        // Nothing changed, so nothing is claimed.
        _ => "No content.".to_string(),
    }
}

/// Put one path parameter's value into the template.
///
/// An optional parameter that fills a whole segment (`/{overlay}` is the
/// only one any invocation can reach today) has to take its slash with it
/// when the value is empty. Substituting an empty string in place would
/// leave `//`, which the API reads as an empty-but-present segment and
/// answers 404 to — so a plain map image with no overlay would fail, while
/// the same URL without the segment succeeds.
///
/// Parameters that are only part of a segment (`{width}x{height}{format}`)
/// get substituted as-is: an empty value there is just the format's
/// default, which is what "optional" means for them.
///
/// `\` is included because WHATWG treats it as a path separator for
/// special schemes, so `..\..\x` traverses exactly like `../../x` —
/// verified against `reqwest::Url`, not assumed.
const PATH_STRUCTURAL: [char; 4] = ['/', '?', '#', '\\'];

/// One path parameter's value, safe to splice into the URL's path.
///
/// The path is built by substituting into a template
/// (`…/{username}/{style_id}/static/{overlay}/…`), so a value carrying URL
/// syntax used to be able to change which request went out. With the
/// caller's token and the command's method attached, `styles delete
/// '../../x'` aimed a `DELETE` at a path nobody asked for, and
/// `'x?extra=1'` appended a query parameter — the same shape as
/// mapbox/mcp-server's `directions_tool` fix.
///
/// **Only these four structural characters are encoded, deliberately.**
/// Path parameters here carry punctuation on purpose: a static-images
/// overlay is `pin-s+f74e4e(-122.46,37.77)`, `{highRes}` is `@2x`,
/// `{format}` is `.png`, and `{lon},{lat},{zoom}` share one comma-separated
/// segment. Percent-encoding everything outside RFC 3986's unreserved set
/// would rewrite all of that and risk breaking requests that work today.
/// Encoding only the characters that change the URL's shape can't break
/// any request that doesn't already contain one of those four.
///
/// Dot segments are refused, not encoded, because encoding doesn't stop
/// them — WHATWG reads `%2e%2e` as a double-dot segment too, so a value of
/// exactly `..` still climbs a level no matter how it's spelled. Encoding
/// the separators handles the multi-level `../../x` case (it collapses to
/// one segment); this handles the single-level case that's left over.
fn path_segment<'a>(name: &str, value: &'a str) -> Result<Cow<'a, str>> {
    // The URL parser treats `%2e` (either case) as a plain dot.
    let as_dots = value.replace("%2e", ".").replace("%2E", ".");
    if as_dots == "." || as_dots == ".." {
        return Err(CliError::new(
            "invalid_path_parameter",
            format!(
                "`{value}` cannot be used as {name}: a path segment of `.` or `..` would move \
                 the request to a different endpoint."
            ),
        )
        .into());
    }

    if !value.contains(PATH_STRUCTURAL) {
        return Ok(Cow::Borrowed(value));
    }

    let mut encoded = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '/' => encoded.push_str("%2F"),
            '?' => encoded.push_str("%3F"),
            '#' => encoded.push_str("%23"),
            '\\' => encoded.push_str("%5C"),
            other => encoded.push(other),
        }
    }
    Ok(Cow::Owned(encoded))
}

fn substitute_path_param(path: &str, name: &str, value: &str, required: bool) -> String {
    let placeholder = format!("{{{name}}}");
    let segment = format!("/{placeholder}");
    if value.is_empty() && !required && path.contains(&segment) {
        return path.replace(&segment, "");
    }
    path.replace(&placeholder, value)
}

/// A transport failure, with the URL stripped out of it.
///
/// `reqwest::Error`'s `Display` appends " for url (…)", and the access
/// token travels in the query string — so the default rendering puts a
/// live token into an error message that `--output json`, CI logs, and
/// pasted issue reports would all capture verbatim. The `--debug` path
/// already redacts the token for this same reason.
///
/// Stripping the URL loses the only detail reqwest's outermost layer
/// carried, so we walk the source chain to recover what actually went
/// wrong — otherwise every proxy, DNS, TLS, and timeout failure would just
/// read "error sending request" and nothing more.
///
/// A timeout gets its own error code, since it's the one failure here a
/// caller can actually act on: raise the `--timeout` budget. Generic
/// "check your proxy" advice would point them the wrong way.
///
/// Shared with `auth`'s `--verify`, which also reaches an endpoint that
/// takes the token as a query parameter — one redaction covers both.
pub(crate) fn transport_failure(context: &str, err: reqwest::Error) -> CliError {
    // Checked before `without_url`, which builds a new error from this one.
    let ran_out_of_time = err.is_timeout();
    let err = err.without_url();
    let mut message = format!("{context}: {err}");

    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }

    if ran_out_of_time {
        // Its own error code, not just a message variant: this is the one
        // transport failure a script can sensibly react to (retry, or
        // raise the budget), and a script shouldn't have to parse prose to
        // find that out.
        return CliError::new("request_timed_out", message).with_remedy(remedy::for_timeout());
    }

    CliError::new("request_failed", message).with_remedy(remedy::for_transport())
}

/// Points a rejected argument at `--schema`, which describes what the
/// command would have accepted.
///
/// Only reacts to the codes listed below, since this sees every failure
/// `execute` produces: an unreadable `--file` is a filesystem problem, and
/// a 404 isn't an argument problem at all, so offering a schema for either
/// would be advice that doesn't apply.
fn with_schema_action(err: anyhow::Error, op: &Operation) -> anyhow::Error {
    const SCHEMA_ANSWERS: [&str; 3] = ["invalid_data", "conflicting_body", "unsupported_body"];

    match err.downcast::<CliError>() {
        Ok(cli) if SCHEMA_ANSWERS.contains(&cli.code.as_str()) => cli
            .with_remedy(Remedy::default().with_action(Some(remedy::schema_command(op))))
            .into(),
        Ok(cli) => cli.into(),
        Err(other) => other,
    }
}

/// "To see one of these, run …", when the spec describes such a command.
///
/// Built from the operation the caller actually ran, so it can only ever
/// name a command that exists. `accounts list-tokens` gets nothing, since
/// the Tokens API has no way to fetch one token by id — inventing a
/// plausible-looking suggestion here would be worse than staying quiet.
fn detail_hint(op: &Operation) -> Option<String> {
    let detail = op.detail.as_ref()?;
    Some(format!("mapbox {} <{}>", detail.command, detail.parameter))
}

/// Content types whose bytes must reach stdout untouched.
fn is_binary_content_type(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if essence.starts_with("image/")
        || essence.starts_with("audio/")
        || essence.starts_with("video/")
        || essence.starts_with("font/")
    {
        return true;
    }

    // `+json` and `+xml` suffixes (application/geo+json) stay textual.
    matches!(
        essence.as_str(),
        "application/octet-stream"
            | "application/zip"
            | "application/gzip"
            | "application/x-gzip"
            | "application/pbf"
            | "application/x-protobuf"
            | "application/protobuf"
            | "application/vnd.mapbox-vector-tile"
    )
}

/// The file extension to suggest for a response we're refusing to print.
///
/// Guessed from the content type instead of always saying `.png` — telling
/// someone to redirect a glyph range into `out.png` produces a
/// mislabeled file and makes it look like the command misunderstood what
/// it fetched.
fn suggested_extension(content_type: &str) -> &'static str {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    match essence.as_str() {
        "image/png" => ".png",
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/webp" => ".webp",
        "application/zip" => ".zip",
        "application/gzip" | "application/x-gzip" => ".gz",
        "application/pbf" | "application/x-protobuf" | "application/protobuf" => ".pbf",
        "application/vnd.mapbox-vector-tile" => ".mvt",
        other => match other.split_once('/') {
            // `font/woff2` and the like name their own extension.
            Some(("font", subtype)) if !subtype.is_empty() => match subtype {
                "woff" => ".woff",
                "woff2" => ".woff2",
                "ttf" => ".ttf",
                "otf" => ".otf",
                _ => ".bin",
            },
            _ => ".bin",
        },
    }
}

/// Writes raw bytes to stdout, refusing to do so when that's a terminal —
/// same as `curl`, and for the same reason: a few hundred KB of PNG would
/// otherwise scramble the shell.
fn write_binary(body: &[u8], content_type: &str) -> Result<()> {
    use std::io::{IsTerminal, Write};

    let mut stdout = std::io::stdout();
    if stdout.is_terminal() {
        let kind = if content_type.is_empty() {
            "binary"
        } else {
            content_type
        };
        return Err(CliError::new(
            "binary_response",
            format!(
                "Response is {kind} ({} bytes). Refusing to write it to the terminal — \
                 redirect it to a file, e.g. `... > out{}`.",
                body.len(),
                suggested_extension(content_type)
            ),
        )
        .into());
    }

    stdout
        .write_all(body)
        .and_then(|()| stdout.flush())
        .map_err(|e| anyhow!("Failed to write response: {}", e))
}

#[cfg(test)]
mod tests {
    use super::{
        describe_body, empty_success_line, file_name_of, is_binary_content_type, part_media_type,
        path_segment, payload_of, query_pairs, redacted_url, request_id, resolve_body_source,
        resolve_data, shell_value, substitute_path_param, with_page_context, BodySource, NextPage,
        ResponseHeaders, ACCESS_TOKEN, REQUEST_ID_HEADERS,
    };
    use std::borrow::Cow;

    use crate::http::Payload;
    use crate::output::CliError;
    use crate::spec::{Parameter, RequestBody};

    /// The `CliError` inside a refusal, so a test can check the error code
    /// instead of matching on prose.
    fn refusal(err: anyhow::Error) -> CliError {
        err.downcast::<CliError>().expect("refused with a CliError")
    }

    fn body(content_types: &[&str], multipart_field: Option<&str>) -> RequestBody {
        RequestBody {
            // `resolve_body_source` decides from the flags, not from
            // whether the spec insists on a body — so this doesn't matter.
            required: false,
            content_types: content_types.iter().map(|s| s.to_string()).collect(),
            multipart_field: multipart_field.map(|s| s.to_string()),
        }
    }

    #[test]
    fn the_suggested_filename_matches_what_was_fetched() {
        for (content_type, extension) in [
            ("image/png", ".png"),
            ("image/jpeg", ".jpg"),
            ("application/x-protobuf", ".pbf"),
            ("application/vnd.mapbox-vector-tile", ".mvt"),
            ("application/zip", ".zip"),
            ("font/woff2", ".woff2"),
            ("IMAGE/PNG; charset=binary", ".png"),
            ("application/octet-stream", ".bin"),
            ("", ".bin"),
        ] {
            assert_eq!(
                super::suggested_extension(content_type),
                extension,
                "{content_type}"
            );
        }
    }

    #[test]
    fn images_tiles_fonts_and_archives_are_binary() {
        for ct in [
            "image/png",
            "image/jpeg",
            "image/webp",
            "application/x-protobuf",
            "application/vnd.mapbox-vector-tile",
            "application/octet-stream",
            "application/zip",
            "font/woff2",
        ] {
            assert!(is_binary_content_type(ct), "{ct} should be binary");
        }
    }

    #[test]
    fn json_and_text_are_not_binary() {
        for ct in [
            "application/json",
            "application/geo+json",
            "text/plain",
            "text/html; charset=utf-8",
            "application/xml",
            "",
        ] {
            assert!(!is_binary_content_type(ct), "{ct} should be textual");
        }
    }

    #[test]
    fn parameters_and_casing_do_not_change_the_verdict() {
        assert!(is_binary_content_type("image/png; charset=binary"));
        assert!(is_binary_content_type("IMAGE/PNG"));
        assert!(is_binary_content_type("  application/zip  "));
        assert!(!is_binary_content_type("APPLICATION/JSON; charset=utf-8"));
    }

    #[test]
    fn an_empty_optional_segment_takes_its_slash_with_it() {
        let template = "/styles/v1/user/s/static/{overlay}/{lon},{lat},{zoom}/{width}x{height}";
        assert_eq!(
            substitute_path_param(template, "overlay", "", false),
            "/styles/v1/user/s/static/{lon},{lat},{zoom}/{width}x{height}",
            "`//` reaches the API as an empty segment and answers 404"
        );
        assert_eq!(
            substitute_path_param(template, "overlay", "pin-s(1,2)", false),
            "/styles/v1/user/s/static/pin-s(1,2)/{lon},{lat},{zoom}/{width}x{height}"
        );
    }

    #[test]
    fn an_empty_value_inside_a_segment_stays_empty() {
        // `{highRes}`/`{format}` are part of a segment, not a whole one —
        // empty means the format's default, not something to remove.
        let template = "/styles/v1/user/s/tiles/512/1/2/3{highRes}{format}";
        assert_eq!(
            substitute_path_param(template, "highRes", "", false),
            "/styles/v1/user/s/tiles/512/1/2/3{format}"
        );
    }

    #[test]
    fn a_required_segment_is_left_empty_rather_than_dropped() {
        // Dropping it would send a URL that means something else; leaving
        // it empty reaches the API, which can say what's actually wrong.
        let template = "/v4/{tilesets}/1/2/3.png";
        assert_eq!(
            substitute_path_param(template, "tilesets", "", true),
            "/v4//1/2/3.png"
        );
    }

    /// `starFile`'s body is the literal word `true`, under `text/plain`.
    /// It's sent as typed — parsing it as JSON and re-encoding would round
    /// it through a format the endpoint rejects.
    #[test]
    fn a_text_body_is_sent_as_typed() {
        let plain = body(&["text/plain"], None);
        assert_eq!(
            resolve_body_source(&plain, Some("true"), &[]).unwrap(),
            BodySource::Text {
                data: "true",
                content_type: "text/plain"
            }
        );
    }

    #[test]
    fn a_json_body_still_goes_through_data() {
        let json = body(&["application/json"], None);
        assert_eq!(
            resolve_body_source(&json, Some("{}"), &[]).unwrap(),
            BodySource::Json("{}")
        );
    }

    /// A 204 only says that it worked; the HTTP method is what makes the
    /// line specific.
    #[test]
    fn an_empty_success_says_what_happened() {
        assert_eq!(
            empty_success_line("DELETE", Some("ckstyle1")),
            "Deleted ckstyle1."
        );
        assert_eq!(empty_success_line("delete", None), "Deleted.");
        assert_eq!(
            empty_success_line("PATCH", Some("folder-1")),
            "Done folder-1."
        );
        assert_eq!(
            empty_success_line("POST", Some("ckstyle1")),
            "Done ckstyle1."
        );
        assert_eq!(empty_success_line("PUT", None), "Done.");
    }

    /// The session endpoints answer 200 with an empty body but change no
    /// state, so the line must not claim they did.
    #[test]
    fn an_empty_get_claims_nothing() {
        assert_eq!(empty_success_line("GET", None), "No content.");
        assert_eq!(empty_success_line("GET", Some("ignored")), "No content.");
    }

    /// The spec's media type must reach the wire verbatim. Sending SVG as
    /// `application/json` is exactly the bug that made
    /// `upload-sprite-image` unusable while still looking like it worked.
    #[test]
    fn a_raw_body_is_sent_as_the_type_the_spec_declared() {
        let svg = body(&["image/svg+xml"], None);
        assert_eq!(
            resolve_body_source(&svg, None, &["icon.svg"]).unwrap(),
            BodySource::Raw {
                path: "icon.svg",
                content_type: "image/svg+xml",
            }
        );

        let bytes = body(&["application/octet-stream"], None);
        assert_eq!(
            resolve_body_source(&bytes, None, &["chunk.bin"]).unwrap(),
            BodySource::Raw {
                path: "chunk.bin",
                content_type: "application/octet-stream",
            }
        );
    }

    #[test]
    fn multipart_files_go_under_the_field_the_spec_names() {
        let form = body(&["multipart/form-data"], Some("images"));
        assert_eq!(
            resolve_body_source(&form, None, &["a.svg", "b.svg"]).unwrap(),
            BodySource::Multipart {
                paths: vec!["a.svg", "b.svg"],
                field: "images",
            }
        );
    }

    #[test]
    fn a_json_body_still_comes_from_data() {
        let json = body(&["application/json"], None);
        assert_eq!(
            resolve_body_source(&json, Some("[\"a\"]"), &[]).unwrap(),
            BodySource::Json("[\"a\"]")
        );
    }

    /// An operation that takes a body but got none still sends the
    /// request — whether the body was actually required is the API's
    /// answer to give, not ours.
    #[test]
    fn no_flag_at_all_sends_an_empty_body() {
        let json = body(&["application/json"], None);
        assert_eq!(
            resolve_body_source(&json, None, &[]).unwrap(),
            BodySource::Empty
        );
    }

    /// Picking one flag silently would discard what the caller asked for
    /// with the other.
    #[test]
    fn data_and_file_together_are_refused() {
        let both = body(&["application/octet-stream", "application/json"], None);
        let err = refusal(resolve_body_source(&both, Some("{}"), &["chunk.bin"]).unwrap_err());
        assert_eq!(err.code, "conflicting_body");
        assert!(err.message.contains("--data"), "{}", err.message);
        assert!(err.message.contains("--file"), "{}", err.message);
    }

    /// `--file` is never offered on a JSON-only operation, but the
    /// resolver still shouldn't depend on clap having enforced that.
    #[test]
    fn a_file_for_a_json_only_body_names_the_flag_that_works() {
        let json = body(&["application/json"], None);
        let err = refusal(resolve_body_source(&json, None, &["thing.json"]).unwrap_err());
        assert_eq!(err.code, "unsupported_body");
        assert!(err.message.contains("--data"), "{}", err.message);
    }

    #[test]
    fn a_raw_body_takes_one_file_not_several() {
        let svg = body(&["image/svg+xml"], None);
        let err = refusal(resolve_body_source(&svg, None, &["a.svg", "b.svg"]).unwrap_err());
        assert_eq!(err.code, "conflicting_body");
    }

    /// `initUpload` declares octet-stream *and* JSON: the non-JSON type is
    /// what `--file` sends, and `--data` still sends the JSON half.
    #[test]
    fn a_body_declaring_both_supports_each_flag_on_its_own() {
        let both = body(&["application/octet-stream", "application/json"], None);
        assert_eq!(
            resolve_body_source(&both, None, &["chunk.bin"]).unwrap(),
            BodySource::Raw {
                path: "chunk.bin",
                content_type: "application/octet-stream",
            }
        );
        assert_eq!(
            resolve_body_source(&both, Some("{}"), &[]).unwrap(),
            BodySource::Json("{}")
        );
    }

    /// The sprite API names each icon after its part's filename, so the
    /// leading directories can't come along.
    #[test]
    fn a_part_is_named_by_the_file_not_its_path() {
        assert_eq!(
            file_name_of("/tmp/sprite/zz-clitest-1.svg"),
            "zz-clitest-1.svg"
        );
        assert_eq!(file_name_of("zz-clitest-1.svg"), "zz-clitest-1.svg");
        assert_eq!(file_name_of("./a/b/c.svg"), "c.svg");
    }

    #[test]
    fn a_parts_media_type_comes_from_its_extension() {
        assert_eq!(part_media_type("icon.svg"), "image/svg+xml");
        assert_eq!(part_media_type("ICON.SVG"), "image/svg+xml");
        assert_eq!(part_media_type("icon.png"), "image/png");
        assert_eq!(part_media_type("blob"), "application/octet-stream");
        assert_eq!(
            part_media_type("archive.tar.gz"),
            "application/octet-stream"
        );
    }

    /// The token travels in the query string, so this rendering function
    /// is all that stands between a live credential and `--debug`'s
    /// stderr or a dry run's stdout.
    #[test]
    fn the_rendered_url_never_carries_the_token() {
        let query = [
            ("access_token".to_string(), "sk.a-real-token".to_string()),
            ("draft".to_string(), "true".to_string()),
        ];
        let rendered = redacted_url("https://api.mapbox.com/styles/v1/me", &query);

        assert!(!rendered.contains("sk.a-real-token"), "{rendered}");
        assert_eq!(
            rendered,
            "https://api.mapbox.com/styles/v1/me?access_token=<redacted>&draft=true"
        );
    }

    /// The printed URL has to *be* a URL.
    ///
    /// Found by running the thing: the Search Box API takes free text now, so
    /// `--q "Dog friendly coffee shops near me"` is an ordinary call, and the
    /// URL beside it on stderr came out with literal spaces. `curl` answers
    /// that with nothing at all — exit 3, no request made — which makes a
    /// debugging aid useless for debugging.
    ///
    /// The request itself was always fine; reqwest encodes what it sends. It
    /// was only this rendering, which is also the dry run's.
    #[test]
    fn a_free_text_query_renders_a_url_that_can_be_used() {
        let query = [
            ("access_token".to_string(), "sk.a-real-token".to_string()),
            (
                "q".to_string(),
                "Dog friendly coffee shops near me".to_string(),
            ),
            ("proximity".to_string(), "-77.0336,38.8996".to_string()),
        ];
        let rendered = redacted_url("https://api.mapbox.com/search/searchbox/v1/forward", &query);

        assert!(
            rendered.contains("q=Dog%20friendly%20coffee%20shops%20near%20me"),
            "{rendered}"
        );
        // A comma is legal in a query and carries meaning to a reader, so it
        // survives: `proximity=-77.0336%2C38.8996` would be correct and worse.
        assert!(
            rendered.contains("proximity=-77.0336,38.8996"),
            "{rendered}"
        );
        assert!(!rendered.contains("sk.a-real-token"), "{rendered}");

        // The claim, checked rather than eyeballed: it parses, and every value
        // comes back out the way it went in.
        let parsed = reqwest::Url::parse(&rendered).expect("the rendered URL has to parse");
        let back: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            back,
            vec![
                ("access_token".to_string(), "<redacted>".to_string()),
                (
                    "q".to_string(),
                    "Dog friendly coffee shops near me".to_string()
                ),
                ("proximity".to_string(), "-77.0336,38.8996".to_string()),
            ]
        );
    }

    /// A value cannot invent a parameter that was never sent.
    ///
    /// This is the half that is worse than ugly. Concatenated raw, a value
    /// holding `&` splits into another `name=value` pair, so the line claims
    /// the request carried something it did not — and anyone who pastes it
    /// sends a different request than the one being debugged. `=` and `+`
    /// are here for the same reason: one changes where a value starts, and
    /// the other is read as a space by anything parsing a form.
    #[test]
    fn a_value_cannot_forge_another_query_parameter() {
        let query = [(
            "q".to_string(),
            "coffee&limit=99&access_token=sk.theirs".to_string(),
        )];
        let rendered = redacted_url("https://api.mapbox.com/search/searchbox/v1/forward", &query);

        let parsed = reqwest::Url::parse(&rendered).expect("parses");
        let names: Vec<String> = parsed.query_pairs().map(|(k, _)| k.into_owned()).collect();
        assert_eq!(names, vec!["q"], "one parameter went in: {rendered}");

        let (_, value) = parsed.query_pairs().next().expect("the one pair");
        assert_eq!(value, "coffee&limit=99&access_token=sk.theirs");
    }

    /// An unauthenticated call has no query at all, so a trailing `?`
    /// would misrepresent the request.
    #[test]
    fn a_url_with_no_query_keeps_no_question_mark() {
        assert_eq!(
            redacted_url("https://api.mapbox.com/styles/v1/me", &[]),
            "https://api.mapbox.com/styles/v1/me"
        );
    }

    /// A dry run promises that what it accepts, a real call would send —
    /// so it must reject the same invalid JSON the real send would, not
    /// let it pass and fail later.
    #[test]
    fn a_dry_run_rejects_the_json_that_sending_would_reject() {
        let err = describe_body(&BodySource::Json("{oops")).expect_err("invalid JSON is refused");
        assert_eq!(refusal(err).code, "invalid_data");
    }

    /// Reading the file (not just stat-ing it) is what catches this —
    /// a metadata check would wave an unreadable file through.
    #[test]
    fn a_dry_run_rejects_a_file_that_cannot_be_read() {
        let err = describe_body(&BodySource::Raw {
            path: "/nonexistent/zz-clitest.svg",
            content_type: "image/svg+xml",
        })
        .expect_err("an unreadable file is refused");
        assert_eq!(refusal(err).code, "invalid_file");
    }

    /// A "Body:" line on every `delete` would just be noise.
    #[test]
    fn a_body_that_was_never_given_is_not_described() {
        assert!(describe_body(&BodySource::Empty)
            .expect("an absent body is not an error")
            .is_none());
    }

    /// `batchUploadSprite` takes each icon's name from its filename, so
    /// the plan should spell that out rather than leave the reader to
    /// work out that `icons/foo.svg` becomes `foo.svg`.
    #[test]
    fn a_multipart_plan_names_each_part() {
        // `CARGO_TARGET_TMPDIR` isn't set here (it's an integration-test
        // variable); the pid keeps concurrent runs on one CI box apart.
        let dir = std::env::temp_dir().join(format!("mapbox-cli-dry-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let icon = dir.join("zz-clitest-1.svg");
        std::fs::write(&icon, b"<svg/>").expect("write icon");
        let path = icon.to_str().expect("utf-8 temp path");

        let described = describe_body(&BodySource::Multipart {
            paths: vec![path],
            field: "images",
        })
        .expect("a readable file describes")
        .expect("a multipart body is described");

        assert!(
            described.text.contains("zz-clitest-1.svg"),
            "{}",
            described.text
        );
        assert_eq!(described.json["field"], "images");
        assert_eq!(described.json["bytes"], 6);
        assert_eq!(described.json["files"][0]["name"], "zz-clitest-1.svg");
        assert_eq!(described.json["files"][0]["content_type"], "image/svg+xml");

        std::fs::remove_dir_all(&dir).expect("clean up temp dir");
    }

    /// Uses a real `reqwest` timeout instead of a hand-made error, because
    /// what's being tested is that `is_timeout()` still answers true after
    /// `transport_failure` strips the URL out — the token rides in that
    /// URL, so the stripping must not break the classification.
    #[test]
    fn a_budget_that_runs_out_says_so_rather_than_blaming_the_network() {
        // Accepts and never answers, and stays held open — dropped, the
        // port closes and the client gets a refusal instead of a hang.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");

        let failure = crate::http::client()
            .expect("a client")
            .get(format!("http://{addr}/"))
            .timeout(std::time::Duration::from_millis(250))
            .send()
            .expect_err("a server that never answers cannot have answered");

        let reported = super::transport_failure("Request failed", failure);
        assert_eq!(reported.code, "request_timed_out");
        let fix = reported.fix.as_deref().unwrap_or_default();
        assert!(
            fix.contains("--timeout"),
            "a timeout was reported without naming the flag that moves it: {fix}"
        );
        assert!(
            !reported.message.contains(&addr.to_string()),
            "the URL survived into the message: {}",
            reported.message
        );
    }

    /// A `--data` body is bounded by what a command line can carry, so
    /// giving `create-style` a fifteen-minute budget would only mean
    /// waiting that long to find out the API is down.
    #[test]
    fn only_a_file_is_treated_as_a_transfer() {
        let raw = BodySource::Raw {
            path: "sprite.svg",
            content_type: "image/svg+xml",
        };
        let multipart = BodySource::Multipart {
            paths: vec!["a.svg", "b.svg"],
            field: "images",
        };
        assert_eq!(payload_of(Some(&raw), false), Payload::File);
        assert_eq!(payload_of(Some(&multipart), false), Payload::File);

        let text = BodySource::Text {
            data: "true",
            content_type: "text/plain",
        };
        assert_eq!(payload_of(None, false), Payload::Bounded);
        assert_eq!(
            payload_of(Some(&BodySource::Empty), false),
            Payload::Bounded
        );
        assert_eq!(
            payload_of(Some(&BodySource::Json("{}")), false),
            Payload::Bounded
        );
        assert_eq!(payload_of(Some(&text), false), Payload::Bounded);
    }

    /// A body read from `@path`/`@-` looks identical to a typed one by the
    /// time it reaches `BodySource::Json`, and nothing bounds its size. On
    /// the ordinary sixty-second budget, a large one would time out in a
    /// way that blames the network instead of the choice of flag.
    #[test]
    fn a_data_body_that_was_read_gets_the_transfer_budget() {
        assert_eq!(
            payload_of(Some(&BodySource::Json("{}")), true),
            Payload::File
        );
        let text = BodySource::Text {
            data: "true",
            content_type: "text/plain",
        };
        assert_eq!(payload_of(Some(&text), true), Payload::File);
    }

    #[test]
    fn a_plain_data_argument_is_the_body_itself() {
        let resolved = resolve_data(r#"{"version":8}"#).expect("a literal body");
        assert_eq!(resolved.body, r#"{"version":8}"#);
        assert!(!resolved.streamed, "argv bounds it");
    }

    #[test]
    fn an_at_path_is_read_from_disk() {
        let dir = tempdir();
        let path = dir.join("style.json");
        std::fs::write(&path, "{\"version\":8}\n").expect("write the style");

        let spec = format!("@{}", path.display());
        let resolved = resolve_data(&spec).expect("the file is read");
        assert_eq!(resolved.body, "{\"version\":8}\n");
        assert!(resolved.streamed, "nothing bounds a file");
    }

    /// A trailing newline a text editor leaves is *not* stripped — every
    /// JSON parser ignores it anyway, and trimming it would mean this CLI
    /// quietly editing what the caller asked to send.
    #[test]
    fn a_read_body_is_sent_byte_for_byte() {
        let dir = tempdir();
        let path = dir.join("body.json");
        std::fs::write(&path, "  {\"a\": 1}  \n\n").expect("write the body");

        let spec = format!("@{}", path.display());
        let resolved = resolve_data(&spec).expect("read");
        assert_eq!(resolved.body, "  {\"a\": 1}  \n\n");
    }

    #[test]
    fn a_missing_at_path_names_the_path_rather_than_crashing() {
        let cli = refusal(resolve_data("@/no/such/style.json").unwrap_err());
        assert_eq!(cli.code, "invalid_file");
        assert!(
            cli.message.contains("/no/such/style.json"),
            "{}",
            cli.message
        );
    }

    /// A JSON body has to be UTF-8, so this is a validity check, not just
    /// a convenience — the alternative is a lossy conversion the API
    /// would then reject for reasons that don't point back at the mistake.
    #[test]
    fn a_non_utf8_file_says_so_rather_than_being_mangled() {
        let dir = tempdir();
        let path = dir.join("bytes.json");
        std::fs::write(&path, [0x7b, 0xff, 0xfe, 0x7d]).expect("write the bytes");

        let spec = format!("@{}", path.display());
        let cli = refusal(resolve_data(&spec).unwrap_err());
        assert_eq!(cli.code, "invalid_file");
        assert!(cli.message.contains("not valid UTF-8"), "{}", cli.message);
    }

    /// Without this, the symptom is "EOF while parsing a value" — naming
    /// the parser instead of the empty pipe that's almost always the real
    /// cause.
    #[test]
    fn an_empty_file_says_which_source_was_empty() {
        let dir = tempdir();
        let path = dir.join("empty.json");
        std::fs::write(&path, "   \n").expect("write whitespace");

        let spec = format!("@{}", path.display());
        let cli = refusal(resolve_data(&spec).unwrap_err());
        assert_eq!(cli.code, "invalid_data");
        assert!(cli.message.contains("was empty"), "{}", cli.message);
        assert!(cli.message.contains("empty.json"), "{}", cli.message);
    }

    #[test]
    fn a_bare_at_sign_names_both_forms() {
        let cli = refusal(resolve_data("@").unwrap_err());
        assert_eq!(cli.code, "invalid_data");
        assert!(cli.message.contains("@<path>"), "{}", cli.message);
        assert!(cli.message.contains("@-"), "{}", cli.message);
    }

    /// Only the first character decides whether this is a path — which is
    /// what makes `--data '{"a":"b@c"}'` safe.
    #[test]
    fn an_at_sign_inside_the_body_is_not_a_path() {
        let resolved = resolve_data(r#"{"email":"a@b.example"}"#).expect("a literal body");
        assert!(!resolved.streamed);
        assert_eq!(resolved.body, r#"{"email":"a@b.example"}"#);
    }

    /// A scratch directory that cleans itself up, so tests leave nothing
    /// behind and can't collide with each other.
    fn tempdir() -> TempDir {
        let base = std::env::temp_dir().join(format!(
            "mapbox-cli-data-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&base).expect("create a scratch directory");
        TempDir(base)
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn join(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A declared query parameter, with only the fields these tests read set
    /// to anything meaningful.
    fn param(name: &str) -> Parameter {
        Parameter {
            name: name.to_string(),
            arg_name: name.to_string(),
            required: false,
            description: None,
            enum_values: vec![],
            is_boolean: false,
            numeric: None,
        }
    }

    #[test]
    fn the_page_tip_names_the_flags_the_spec_declares() {
        let declared = [param("start"), param("limit")];
        let next = "https://api.mapbox.com/styles/v1/u?start=cjk2&limit=10";

        let tip = NextPage::of(&declared, next).tip();
        assert!(tip.contains("--start cjk2"), "{tip}");
        assert!(tip.contains("--limit 10"), "{tip}");
    }

    /// A parameter the API sent back but this command doesn't declare has
    /// no flag to name, so it's left out instead of invented.
    #[test]
    fn an_undeclared_query_parameter_is_not_named() {
        let declared = [param("start")];
        let next = "https://api.mapbox.com/a?start=7&extra=1";

        let tip = NextPage::of(&declared, next).tip();
        assert!(tip.contains("--start 7"), "{tip}");
        assert!(!tip.contains("extra"), "{tip}");
    }

    /// **The security property of this whole path.**
    ///
    /// The access token rides in the query string, so the `Link` URL the
    /// API echoes back contains a live token. It's excluded by
    /// construction — `dispatch` adds it directly rather than declaring it
    /// in `op.query_params`, and only declared parameters become flags —
    /// but that's worth testing directly, since being wrong here means
    /// printing a credential to a terminal (and to whatever captured it).
    #[test]
    fn the_page_tip_never_names_the_access_token() {
        let secret = "pk.eyJ1IjoibWFwYm94IiwiYSI6ImNqa2xpdmV0b2tlbiJ9.aaaaaaaaaaaaaaaaaaaaaa";
        let declared = [param("start"), param("limit")];
        let next =
            format!("https://api.mapbox.com/styles/v1/u?access_token={secret}&start=cjk2&limit=10");

        let page = NextPage::of(&declared, &next);
        for rendered in [page.tip(), page.fix()] {
            assert!(!rendered.contains(secret), "leaked the token: {rendered}");
            assert!(!rendered.contains("access_token"), "{rendered}");
            assert!(!rendered.contains("pk.ey"), "{rendered}");
        }
    }

    /// Even if a spec later declares a parameter literally named
    /// `access_token` — the way the guarantee above could be undone from
    /// the spec side instead of this file.
    #[test]
    fn a_declared_parameter_named_access_token_is_still_withheld() {
        let declared = [param(ACCESS_TOKEN), param("start")];
        let next = "https://api.mapbox.com/a?access_token=pk.secret&start=3";

        let tip = NextPage::of(&declared, next).tip();
        assert!(!tip.contains("pk.secret"), "{tip}");
        assert!(tip.contains("--start 3"), "{tip}");
    }

    /// Decoded because the CLI re-encodes whatever it's given: handing
    /// back `%2B` would round-trip to `%252B` and fetch the wrong page.
    #[test]
    fn the_page_tip_decodes_percent_escapes() {
        let declared = [param("start")];
        let next = "https://api.mapbox.com/a?start=a%2Bb";

        let tip = NextPage::of(&declared, next).tip();
        assert!(tip.contains("--start a+b"), "{tip}");
    }

    /// A value with a space in it would silently become two arguments if
    /// pasted unquoted.
    #[test]
    fn a_value_needing_a_shell_quote_gets_one() {
        assert_eq!(shell_value("cjk2ab"), "cjk2ab");
        assert_eq!(shell_value("2026-09-01"), "2026-09-01");
        assert_eq!(shell_value("a b"), "'a b'");
        assert_eq!(shell_value(""), "''");
        assert_eq!(shell_value("it's"), r"'it'\''s'");
    }

    /// The result is incomplete either way, so this should still say so —
    /// without claiming a flag that doesn't exist.
    #[test]
    fn no_declared_paging_parameter_still_says_the_result_is_partial() {
        let tip = NextPage::of(&[param("unrelated")], "https://api.mapbox.com/a?start=3").tip();
        assert!(tip.contains("More results exist"), "{tip}");
        assert!(!tip.contains("--"), "{tip}");
    }

    /// An unparseable `Link` target costs nothing — the request succeeded,
    /// and a tip isn't worth turning that into a failure.
    #[test]
    fn an_unparseable_next_url_yields_no_flags() {
        assert!(query_pairs("not a url").is_empty());
        assert!(query_pairs("").is_empty());
    }

    #[test]
    fn response_headers_read_the_three_things_that_survive() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        map.insert(REQUEST_ID_HEADERS[0], "req-abc123".parse().unwrap());
        map.insert(
            reqwest::header::LINK,
            r#"<https://api.mapbox.com/a?start=3>; rel="next""#.parse().unwrap(),
        );

        let headers = ResponseHeaders::read(&map);
        assert_eq!(headers.content_type, "application/json");
        assert_eq!(headers.request_id.as_deref(), Some("req-abc123"));
        assert_eq!(
            headers.next_page.as_deref(),
            Some("https://api.mapbox.com/a?start=3")
        );
    }

    /// The header that actually arrives in practice.
    ///
    /// No Mapbox endpoint reachable from here sends `x-request-id` —
    /// styles, tokens, fonts, and geocoding v6 were all checked, on
    /// success and on a 404. All of them send `x-amz-cf-id` instead, since
    /// the API sits behind CloudFront. Checking only the conventional name
    /// would make this whole feature inert, which is what this test guards
    /// against.
    #[test]
    fn the_cloudfront_id_is_read_when_there_is_no_request_id() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(
            "x-amz-cf-id",
            "E5Kat8az0mUkEYObB4Nvhm6Bi49kt50A".parse().unwrap(),
        );

        assert_eq!(
            request_id(&map).as_deref(),
            Some("E5Kat8az0mUkEYObB4Nvhm6Bi49kt50A")
        );
    }

    /// A service that sends its own id is more specific than the CDN in
    /// front of it.
    #[test]
    fn an_explicit_request_id_outranks_the_cloudfront_one() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert("x-amz-cf-id", "cloudfront".parse().unwrap());
        map.insert("x-request-id", "from-the-service".parse().unwrap());

        assert_eq!(request_id(&map).as_deref(), Some("from-the-service"));
    }

    /// A response with neither claims nothing.
    #[test]
    fn no_identifying_header_is_none() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        assert_eq!(request_id(&map), None);
    }

    /// A present-but-blank header says nothing; an empty request id would
    /// print as `Request ID:` with nothing after it.
    #[test]
    fn a_blank_header_reads_as_absent() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(REQUEST_ID_HEADERS[0], "   ".parse().unwrap());
        map.insert(reqwest::header::LINK, "".parse().unwrap());

        let headers = ResponseHeaders::read(&map);
        assert_eq!(headers.content_type, "");
        assert_eq!(headers.request_id, None);
        assert_eq!(headers.next_page, None);
    }

    /// The last page of a listing still carries a `Link`, naming the pages
    /// behind it. "Has a header" must not read as "has more".
    #[test]
    fn a_link_without_a_next_relation_is_not_another_page() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert(
            reqwest::header::LINK,
            r#"<https://api.mapbox.com/a?start=1>; rel="prev""#.parse().unwrap(),
        );
        assert_eq!(ResponseHeaders::read(&map).next_page, None);
    }

    /// On a paginated response, "No row has the id" is true of this page
    /// but may be false of the whole listing — the most misleading form
    /// of truncation this path exists to prevent.
    #[test]
    fn an_id_miss_on_a_paginated_listing_says_the_row_may_be_later() {
        let page = NextPage::of(&[param("start")], "https://api.mapbox.com/a?start=3");
        let err = with_page_context(
            CliError::new("not_found", "No row has the id `x`.").into(),
            Some(&page),
        );

        let cli = err.downcast_ref::<CliError>().expect("still a CliError");
        let fix = cli.fix.as_deref().expect("a fix was added");
        assert!(fix.contains("one page of results"), "{fix}");
        assert!(fix.contains("--start 3"), "{fix}");
    }

    /// `not_a_list` is about the shape of the response, which another page
    /// would not change — so it keeps its own advice.
    #[test]
    fn a_shape_error_is_not_given_paging_advice() {
        let page = NextPage::of(&[param("start")], "https://api.mapbox.com/a?start=3");
        let err = with_page_context(
            CliError::new("not_a_list", "`--id` only applies to a list.").into(),
            Some(&page),
        );
        assert_eq!(err.downcast_ref::<CliError>().unwrap().fix, None);
    }

    /// An unpaginated response leaves every error exactly as it was.
    #[test]
    fn without_another_page_an_error_passes_through_untouched() {
        let err = with_page_context(
            CliError::new("not_found", "No row has the id `x`.").into(),
            None,
        );
        assert_eq!(err.downcast_ref::<CliError>().unwrap().fix, None);
    }

    /// The shape mapbox/mcp-server fixed in `directions_tool`: a spliced-in
    /// value used to append query parameters the caller never asked for.
    /// Verified against `reqwest::Url`: `x?extra=1` gave
    /// `query = extra=1&access_token=…`.
    #[test]
    fn a_path_parameter_cannot_inject_a_query_string() {
        let safe = path_segment("style_id", "x?extra=1").expect("encoded, not refused");
        assert_eq!(safe, "x%3Fextra=1");
    }

    /// With the caller's token and method attached, this used to aim a
    /// `DELETE` at whatever path the value resolved to. The host was never
    /// reachable this way (`//evil`, `https://evil`, `x@evil` all stay on
    /// `api.mapbox.com`), but the path was.
    #[test]
    fn a_path_parameter_cannot_retarget_the_path() {
        let safe = path_segment("style_id", "../../tokens/v2/victim").expect("encoded");
        assert_eq!(safe, "..%2F..%2Ftokens%2Fv2%2Fvictim");
        assert!(!safe.contains('/'), "one segment, not four: {safe}");
    }

    /// `\` is also a path separator for special schemes — WHATWG says so,
    /// and `reqwest::Url` agrees: `..\..\tokens` resolves just like
    /// `../../tokens`. Encoding `/` alone would leave this open.
    #[test]
    fn a_backslash_cannot_retarget_the_path_either() {
        let safe = path_segment("style_id", r"..\..\tokens").expect("encoded");
        assert_eq!(safe, "..%5C..%5Ctokens");
    }

    /// A fragment is never sent to the server, so this used to silently
    /// truncate the path instead of redirecting it — a request to
    /// somewhere the caller couldn't see from what they typed.
    #[test]
    fn a_fragment_cannot_truncate_the_path() {
        assert_eq!(
            path_segment("style_id", "x#frag").expect("encoded"),
            "x%23frag"
        );
    }

    /// **Refused, not encoded — that distinction is the point.** Encoding
    /// doesn't stop a dot segment: WHATWG reads `%2e%2e` as one too, so a
    /// value of exactly `..` climbs a level no matter how it's spelled.
    /// Encoding the separators handles the multi-level case (it collapses
    /// into one segment); this handles what's left over.
    #[test]
    fn a_dot_segment_is_refused_however_it_is_spelled() {
        for value in ["..", ".", "%2e%2e", "%2E%2E", "%2e", ".%2e", "%2e."] {
            let err = path_segment("style_id", value).expect_err(value);
            assert_eq!(refusal(err).code, "invalid_path_parameter", "{value}");
        }
    }

    /// Encoded dots *and* encoded slashes together — looks like traversal,
    /// isn't.
    ///
    /// `%2f` is never decoded into a real separator by the URL parser, so
    /// this stays one segment on the wire — unlike encoded dots with
    /// *raw* slashes (`%2e%2e/%2e%2e/x`), which the parser does resolve,
    /// and which the encoding above already stops. Left alone on purpose:
    /// percent-encoded values are a documented Mapbox feature, not an
    /// attack signature. A custom marker overlay is
    /// `url-https%3A%2F%2Fexample.com%2Fmarker.png(…)`, `geojson(…)` takes
    /// URI-encoded GeoJSON, and the spec says a bbox's brackets "may be
    /// sent literally or percent-encoded as `%5B`". Refusing these would
    /// break all three.
    #[test]
    fn a_percent_encoded_separator_stays_one_segment() {
        let value = "%2e%2e%2fvictim";
        let safe = path_segment("style_id", value).expect("not refused");
        assert_eq!(safe, value, "passed through, because it names one segment");

        let url = reqwest::Url::parse(&format!("https://api.mapbox.com/styles/v1/u/{safe}"))
            .expect("parses");
        let segments: Vec<&str> = url
            .path_segments()
            .map(Iterator::collect)
            .unwrap_or_default();
        assert_eq!(
            segments,
            ["styles", "v1", "u", "%2e%2e%2fvictim"],
            "one segment under /u/, not a traversal: {}",
            url.path()
        );
    }

    /// **Why this encodes only four characters, not everything outside
    /// RFC 3986's unreserved set.** These values carry punctuation on
    /// purpose, and percent-encoding it would rewrite requests that work
    /// today — `static get-image`'s template alone is
    /// `…/static/{overlay}/{lon},{lat},{zoom},{bearing},{pitch}/{width}x{height}{highRes}{format}`.
    #[test]
    fn punctuation_a_path_parameter_legitimately_carries_is_untouched() {
        for value in [
            "pin-s+f74e4e(-122.46,37.77)",
            "@2x",
            ".png",
            "-122.4194,37.7749,12,0,0",
            "mapbox.mapbox-streets-v8",
            "Arial Unicode MS Regular",
            "0-255",
            "ckstyle00000000000000001a",
        ] {
            let safe = path_segment("p", value).expect("not refused");
            assert_eq!(safe, value, "should pass through byte for byte");
            assert!(matches!(safe, Cow::Borrowed(_)), "and without allocating");
        }
    }

    /// Several operations rely on an empty optional parameter still
    /// dropping its whole segment — encoding must not break that.
    #[test]
    fn an_empty_optional_parameter_still_drops_its_segment() {
        let safe = path_segment("draft", "").expect("empty is not a dot segment");
        assert_eq!(safe, "");
        assert_eq!(
            substitute_path_param("/styles/v1/{u}/{id}/draft", "draft", &safe, false),
            "/styles/v1/{u}/{id}/draft"
        );
        assert_eq!(
            substitute_path_param("/styles/v1/{u}/{id}/{draft}", "draft", &safe, false),
            "/styles/v1/{u}/{id}"
        );
    }
}
