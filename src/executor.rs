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

/// The query parameter the access token travels in, and what stands in for it
/// anywhere the URL is shown. Named once because getting this wrong leaks a
/// live token: `--debug` and the dry-run plan both render the same URL.
const ACCESS_TOKEN: &str = "access_token";
const REDACTED: &str = "<redacted>";

/// The media type a `--data` body is sent as, unless the spec named a text one.
const JSON_CONTENT_TYPE: &str = "application/json";

/// Name of the flag that stops short of sending. Declared per operation
/// rather than globally: a `GET` has nothing to preview, and a flag offered
/// where it cannot mean anything is worse than one you have to put after the
/// operation name. See [`dry_run_arg`].
pub const DRY_RUN_ARG: &str = "dry-run";

/// The wording for a command whose mutation is an API call.
pub const DRY_RUN_REQUEST_HELP: &str =
    "Print the request this would send, then exit without sending it";

/// The `--dry-run` flag, with the sentence that fits what the command does.
///
/// The help text is a parameter because the two kinds of mutating command
/// change different things: a generated operation would send a request, while
/// `mapbox auth logout` deletes a file and sends nothing at all. A `--help`
/// line that describes the wrong one is the small kind of lie that stops a
/// safety flag from being reached for.
pub fn dry_run_arg(help: &'static str) -> clap::Arg {
    clap::Arg::new(DRY_RUN_ARG)
        .long(DRY_RUN_ARG)
        .action(clap::ArgAction::SetTrue)
        .help(help)
}

/// Whether `--dry-run` was given, for an operation that may not have it.
///
/// `get_flag` panics on an argument that was never registered, and read-only
/// operations never register this one.
pub fn wants_dry_run(matches: &ArgMatches) -> bool {
    matches
        .try_get_one::<bool>(DRY_RUN_ARG)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

/// The three switches that change whether and how a request goes out, as
/// opposed to what is being requested.
///
/// Grouped once there were three of them. Clippy's argument limit is the
/// visible reason; the better one is that a call site handing over three bare
/// `bool`s in a row is one transposition away from a silent swap — `--debug`
/// for `--dry-run` would send the request it promised only to describe.
/// Naming them at the call site makes that impossible.
#[derive(Clone, Copy)]
pub struct RunFlags {
    pub debug: bool,
    pub assume_yes: bool,
    pub dry_run: bool,
    /// The budget the caller asked for, from `crate::http::requested` —
    /// `--timeout` or `MAPBOX_TIMEOUT`. `None` does not mean "no timeout": it
    /// means nobody said, and [`crate::http::budget`] falls back to whichever
    /// default fits what the request is carrying.
    pub timeout: Option<Duration>,
}

/// Runs one generated command — the request it would make, sent or, under
/// `--dry-run`, described.
///
/// A wrapper, so that every way this can reject an *argument* is pointed at
/// `--schema` from one place instead of three: the choice between `--data`
/// and `--file`, a `--data` that will not parse, and the same parse again on
/// the dry-run path, which reads the body rather than trusting it.
/// `with_schema_action` answers to a code, so every other failure — an HTTP
/// status, an unreadable file — passes through untouched.
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

    // Substitute {username}/{owner}/{account} from global --username
    if let Some(u) = username {
        for placeholder in ACCOUNT_PLACEHOLDERS {
            path = path.replace(&format!("{{{placeholder}}}"), u);
        }
    }

    // Substitute other path params from positional args
    for param in &op.path_params {
        if let Some(val) = matches.get_one::<String>(&param.arg_name) {
            let safe = path_segment(&param.name, val)?;
            path = substitute_path_param(&path, &param.name, &safe, param.required);
        }
    }

    if path.contains('{') {
        // Extract missing param names for a useful error
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
        // The message names the two ways to supply an account; this names
        // the command that says whether a login would supply it already.
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

    // `--data` and `--file` are declared per-operation, so either may be
    // absent from this command entirely; `get_one` panics on an argument
    // that was never registered.
    // Resolved before anything reads it, `--dry-run` included, so a `@path`
    // that does not exist fails here rather than being described as a request
    // and then failing at send time. `resolve_data` says why.
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

    // Resolved ahead of the dry-run branch on purpose: choosing between
    // `--data` and `--file` is a check the caller can fail, and a dry run
    // that skipped it would report a request that could not actually be
    // sent — which is the one thing it exists to rule out.
    let body_source = match &op.body {
        Some(body) => Some(resolve_body_source(body, data, &files)?),
        None => None,
    };

    if dry_run {
        // Worth saying out loud, and only here: a real call answers a missing
        // token with a 401 the caller cannot miss, while a dry run would
        // happily print a plausible-looking request that could never succeed.
        if token.is_none() {
            output::progress(
                "Note: no access token was resolved, so the real request would be unauthenticated.",
            );
        }
        return describe_request(op, mode, &url, &query, body_source.as_ref());
    }

    // Below the dry-run branch, and that ordering is the point: a dry run
    // sends nothing, so asking whether to go ahead would be asking about
    // something that is not going to happen — and it would make the flag that
    // exists to be safe the one that blocks a script.
    //
    // `url` is passed rather than the rendered request because it carries no
    // query string, and the access token is a query parameter. The question is
    // printed. `redacted_url` above exists for the same reason on the same
    // string; this call must never be given its output.
    confirm::destructive_request(&op.method, &url, assume_yes)?;

    // Below the dry-run branch, so a `[debug]` request line always means a
    // request that went out. The plan renders the same URL itself, and one
    // that reads as a call having been made is the last thing this flag
    // should print.
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
    // Named on the request rather than left to the client, because the client
    // cannot know what any one request is doing: a sprite upload sends a file
    // and a listing sends nothing, and a single budget that suits both is
    // either too short for the upload or too long to be a timeout at all.
    // `reqwest` prefers the request's own over the client's — 0.12.28's
    // `execute_request` reads `req.timeout().copied().or(self.timeout.0)` —
    // so this is what actually applies to everything sent from here.
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
    // Read the headers before `bytes()` consumes the response — whatever is
    // not taken here is gone by the next line. `Content-Type` was for a long
    // time the only one that survived this point, which is what made
    // pagination and support escalation unreachable; see [`ResponseHeaders`].
    let headers = ResponseHeaders::read(response.headers());
    let content_type = headers.content_type.clone();
    let body = response
        .bytes()
        .map_err(|e| transport_failure("Failed to read response", e))?;

    // Six of the twelve services answer with bytes, not text — images, vector
    // tiles, glyph PBFs, style ZIPs. Decoding those as UTF-8 replaces every
    // invalid sequence with U+FFFD, which silently corrupts the payload: a PNG
    // arrives with its leading 0x89 rewritten to EF BF BD and no longer opens.
    // An error response is worth reading whatever the endpoint normally
    // returns, so failures always take the text path.
    let as_text = if status.is_success() && is_binary_content_type(&content_type) {
        None
    } else {
        Some(String::from_utf8_lossy(&body))
    };

    // A failure is an error, not a result: it belongs on stderr in whichever
    // shape the caller asked for, so it never lands in a redirected file
    // alongside real output. The body is carried into the error rather than
    // printed here, which keeps every detail the old dump-to-stdout showed.
    if !status.is_success() {
        let text = as_text
            .as_deref()
            .expect("a failure always takes the text path");
        return Err(CliError::http(status.as_u16(), text)
            .with_request_id(headers.request_id)
            .with_remedy(remedy::for_http(status.as_u16(), op, matches, username))
            .into());
    }

    // A 204, or a 200 that carries nothing. `serde_json` cannot parse an
    // empty string, so this fell through to the text-body path and printed a
    // blank line — or `""` under `json`, a valid document that says nothing.
    // Every mutation in the CLI that succeeds without a body landed there,
    // which left a delete indistinguishable from a command that did not run.
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

    // `Some` only when the response is one page of several, which is the
    // listings and nothing else.
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
                    // The row asked for is in hand; where the *other* rows
                    // are is not advice about it.
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
        // Bytes bypass the output contract entirely. A PNG cannot be wrapped
        // in a JSON envelope without destroying it, and `--output json` on a
        // tile endpoint is far more likely to be the global flag riding along
        // than a considered request to mangle the image.
        None => write_binary(&body, &content_type)?,
    }

    Ok(())
}

/// The headers a Mapbox response may identify itself with, in the order they
/// are preferred.
///
/// Measured rather than assumed, and the measurement is the reason there are
/// two. `x-request-id` is the name the convention would predict and is what
/// this looked for first — but no Mapbox endpoint reachable from here sends
/// it: styles, tokens, fonts and geocoding v6 all answer without one, on both
/// success and failure. What every one of them does carry is `x-amz-cf-id`,
/// the CloudFront request id, because the whole API is fronted by it — and
/// that is the id support traces a request with.
///
/// `x-request-id` stays first because a service that does send one means it
/// more specifically than the CDN in front of it does, and it costs a lookup
/// in a map that is already in memory.
const REQUEST_ID_HEADERS: [&str; 2] = ["x-request-id", "x-amz-cf-id"];

/// The request id from a response, for a caller that reads the body itself.
///
/// `text()` and `bytes()` both consume the response, so this has to be called
/// before the body is read — which is the whole reason it is a named function
/// rather than a line inlined at each of the three call sites.
///
/// Mapbox-bound requests only. `agent_skills` talks to GitHub codeload, which
/// identifies requests with `x-github-request-id` and is not something Mapbox
/// support can look up, so it deliberately does not call this.
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
/// A struct rather than three reads at the call site because `bytes()`
/// consumes the response: whatever is not taken before it is unrecoverable.
/// Taking only `Content-Type` is what left paginated listings truncating
/// silently and left a 500 with nothing to quote to support.
struct ResponseHeaders {
    /// Decides whether the body is read as text or written as bytes.
    content_type: String,
    /// The request id — what support needs to find this one request in their
    /// logs. Carried into the error and never printed on success, because on
    /// a response that worked it is noise.
    request_id: Option<String>,
    /// The `rel="next"` target of a `Link` header, when this response is one
    /// page of several.
    next_page: Option<String>,
}

impl ResponseHeaders {
    fn read(headers: &reqwest::header::HeaderMap) -> Self {
        // A header present but empty says nothing, and an empty request id
        // would print as `Request ID:` with a blank after it.
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

/// A response that is one page of several, and how to ask for the next one.
///
/// Holds the flags rather than the URL: following the `Link` target verbatim
/// would mean re-sending a URL the API built, token and all, while the flags
/// are something a caller can read, edit and run.
struct NextPage(Option<String>);

impl NextPage {
    /// Derives the flags from the operation rather than hardcoding `--start`.
    ///
    /// The next URL's query is matched against the parameters this command
    /// declares, so the tip names whatever the spec calls its paging
    /// parameters, and a service that pages some other way needs no change
    /// here.
    ///
    /// **The access token cannot appear in the result.** It rides in the
    /// query string of every request, so the API echoes it back in this very
    /// URL. Two things keep it out, and the second is why the first is not
    /// enough: `dispatch` adds it directly rather than declaring it in
    /// `op.query_params`, so matching against the declared parameters
    /// excludes it today — but a spec is free to declare a parameter by that
    /// name, and then "by construction" would quietly stop being true, so it
    /// is also refused explicitly. `the_page_tip_never_names_the_access_token`
    /// and `a_declared_parameter_named_access_token_is_still_withheld` hold
    /// both halves.
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
            // Reachable only if the API pages an operation whose spec
            // declares no paging parameter — a spec gap, not a user error.
            // Saying so beats saying nothing, because the result is
            // incomplete either way and only this knows it.
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
/// Percent-decoded on purpose: the values go into a tip meant to be copied
/// onto a command line, and the CLI re-encodes whatever it is given — so
/// handing back `%2B` would round-trip to `%252B` and ask for the wrong page.
/// An unparseable URL yields nothing rather than failing: a tip is not worth
/// turning a successful request into an error.
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
/// Paging cursors are opaque ids in practice, but the tip is advice a reader
/// pastes, and an unquoted value with a space in it would silently become
/// two arguments.
fn shell_value(value: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_.~:@+,".contains(c);
    if !value.is_empty() && value.chars().all(safe) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Adds "there are more pages" to a `--id` that matched nothing.
///
/// `pick_row` searches the rows it was handed and says "No row has the id",
/// which is true of the page and may well be false of the listing. On a
/// paginated response that is the most misleading form of the truncation
/// this whole path exists to stop, so the error says which it means.
fn with_page_context(err: anyhow::Error, next_page: Option<&NextPage>) -> anyhow::Error {
    let Some(next_page) = next_page else {
        return err;
    };
    match err.downcast::<CliError>() {
        // `not_a_list` is about the shape of the response, which another
        // page would not change.
        Ok(cli) if cli.code == "not_found" => cli
            .with_remedy(Remedy::default().with_fix(&next_page.fix()))
            .into(),
        Ok(cli) => cli.into(),
        Err(other) => other,
    }
}

/// The request line a reader may safely see: URL, query, token replaced.
///
/// The access token rides in the query string, so every rendering of this URL
/// has to strip it. `--debug` has always done so; the dry run needs it more,
/// because the request *is* its output — the thing most likely to be pasted
/// into an issue or captured whole by CI.
fn redacted_url(url: &str, query: &[(String, String)]) -> String {
    let rendered: Vec<String> = query
        .iter()
        .map(|(name, value)| format!("{name}={}", shown_value(name, value)))
        .collect();

    if rendered.is_empty() {
        url.to_string()
    } else {
        format!("{url}?{}", rendered.join("&"))
    }
}

/// One query parameter's value, or the stand-in when it is the token.
///
/// The single place that decides what may be printed. Both renderings of the
/// query — the URL line and the JSON object — go through it, so the rule
/// cannot come to differ between the two.
fn shown_value<'a>(name: &str, value: &'a str) -> &'a str {
    if name == ACCESS_TOKEN {
        REDACTED
    } else {
        value
    }
}

/// The dry run's answer: what the real call would send, and nothing sent.
///
/// It leaves through `output::emit` like any other result, and it goes to
/// stdout for the same reason — the command was asked what it would do, and
/// this is the answer to that question, not a note about it. So `-o json`
/// gives a script an object to assert on before it lets a delete run for
/// real, and a terminal gets one readable request line.
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

    // An object, not a list of pairs: no query parameter can be repeated
    // here, since each one comes from an argument clap keeps a single value
    // for, and `.query.access_token` is worth more to a caller than
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
            // Without the query string, which `query` carries broken out.
            // Rejoining the two would only produce a URL nobody can use, the
            // token in it being redacted.
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
/// Every check [`attach_body`] makes is made here too: the JSON is parsed,
/// and each `--file` is read rather than stat-ed. Reading is the point. A dry
/// run answers "would this work?", and a file that exists but cannot be read
/// passes a metadata check and fails the real call — which is precisely the
/// surprise the flag exists to rule out. The operations taking `--file` take
/// sprites and upload chunks, so the read costs nothing worth saving.
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
                    // The parsed document rather than the string it was typed
                    // as: re-escaping it into a JSON string would make the one
                    // thing worth checking here the one thing unreadable.
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
                // The part's filename, spelled out because it is not
                // cosmetic: `batchUploadSprite` takes the icon name from it,
                // so this line is what the sprite will be called.
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
/// Borrows rather than owns so the decision stays free of I/O: choosing is
/// separable from reading the files, and only the choosing is worth testing.
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
    /// Whether it came from a file or stdin rather than from argv.
    ///
    /// Carried because the timeout budget turns on it and on nothing else a
    /// caller can see: argv caps what can be typed at roughly a megabyte, and
    /// nothing caps a file. See [`payload_of`].
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
/// itself — the spelling curl has used for long enough that it is what people
/// try first.
///
/// The ambiguity this inherits is curl's: a body whose first character is a
/// literal `@` cannot be passed this way. It costs nothing here, because every
/// operation reachable with `--data` today sends JSON, and `@` is not valid
/// JSON. If a text body that could start with one is ever wired up, `--data-raw`
/// is the established escape hatch.
///
/// Read here rather than at send time so that a `--dry-run` validates the file
/// too. A dry run that skipped this would describe a request that could not
/// actually be sent, which is the one thing it exists to rule out.
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

    // An empty body reaches `attach_body` as invalid JSON and is reported as
    // one — "EOF while parsing a value" — which describes the symptom and not
    // the mistake. The mistake is almost always a pipe that produced nothing
    // (`cat missing.json | mapbox …`, whose own error went to the same stderr
    // and scrolled past), and naming the source is what points at it.
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
/// Text rather than bytes, and that is a check rather than a convenience: a
/// JSON body has to be UTF-8, so a file that is not says so here instead of
/// being lossily converted into a body the API would reject for reasons that
/// name nothing the caller did.
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
/// Pure, and kept that way: every rejection here is a mistake the caller can
/// fix from the message alone, without a request having been sent.
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

    // A raw body is one file by definition. clap keeps only the last
    // occurrence for this operation, so this guards the function, not the CLI.
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

/// How much the request is about to move, which is all the budget turns on.
///
/// `--file` is unbounded, and so is a `--data @path` or `--data @-`, which is
/// why this takes a second argument rather than reading the body alone. A
/// `--data` body *typed* on a command line is capped by argv at a megabyte or
/// so and goes out inside the ordinary budget with room to spare — that was
/// once true of every `--data` body, and the reasoning is the thing `@path`
/// broke: `BodySource::Json` looks identical whether it was typed or read from
/// a 200 MB file, and the second would have been given a sixty-second budget
/// it could not meet.
///
/// The response is not consulted, because nothing here knows it yet: six of
/// the twelve services answer with bytes, but a tile, a glyph range and a
/// style ZIP all arrive well inside a minute, so the one shape worth
/// separating out is the one this CLI is sending.
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
/// A path that does not exist is the most likely thing to go wrong with this
/// flag, and it must read as the caller's typo rather than as a crash.
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
/// It is not cosmetic for sprites: `batchUploadSprite` takes the icon name
/// from each part's filename, so `zz-clitest-1.svg` becomes the icon
/// `zz-clitest-1`.
fn file_name_of(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The media type to label one multipart part with.
///
/// The spec describes the parts as `format: binary` and nothing more, so the
/// extension is the only evidence available. Getting it wrong matters:
/// `batchUploadSprite` rejects a part that does not claim to be SVG.
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
/// The HTTP method is all there is to go on: these commands are generated
/// from the specs and share one code path, so a 204 looks the same whether a
/// style was deleted or a folder renamed. Naming the resource makes the line
/// specific enough to be worth reading, and the last path parameter is the
/// one that identifies it — the earlier ones scope it (`{username}`, then
/// `{style_id}`, then `{icon_name}`).
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
        // A GET that answers 200 with an empty body: the session endpoints.
        // Nothing was changed, so nothing is claimed.
        _ => "No content.".to_string(),
    }
}

/// Put one path parameter's value into the template.
///
/// An optional parameter that occupies a whole segment — `/{overlay}` is
/// the only one any invocation can now reach — has to take its slash with it
/// when the value is empty.
/// Substituting an empty string in place leaves `//`, which the API reads as
/// a segment that is present and empty and answers 404, so asking for a plain
/// map image with no overlay would fail while the same URL without the
/// segment returns it.
///
/// Parameters that are only part of a segment (`{width}x{height}{format}`)
/// are substituted as they are: an empty value there is the format's default,
/// which is what the spec means by optional.
/// The characters that would change the URL's *structure* rather than name a
/// segment within it.
///
/// `\` is here because WHATWG treats it as a path separator for special
/// schemes, so `..\..\x` traverses exactly as `../../x` does — verified
/// against `reqwest::Url`, not assumed.
const PATH_STRUCTURAL: [char; 4] = ['/', '?', '#', '\\'];

/// One path parameter's value, safe to splice into the URL's path.
///
/// The path is built by substituting into a template
/// (`…/{username}/{style_id}/static/{overlay}/…`), so a value carrying URL
/// syntax used to change which request went out. With the caller's token and
/// the command's method attached, `styles delete '../../x'` aimed a `DELETE`
/// at a path nobody asked for, and `'x?fresh=true'` appended a query
/// parameter — the same shape as mapbox/mcp-server's `directions_tool` fix.
///
/// **Only the four structural characters are encoded, deliberately.** Path
/// parameters here carry punctuation on purpose: a static-images overlay is
/// `pin-s+f74e4e(-122.46,37.77)`, `{highRes}` is `@2x`, `{format}` is `.png`,
/// and `{lon},{lat},{zoom}` are three placeholders sharing one comma-
/// separated segment. Percent-encoding everything outside RFC 3986's
/// unreserved set would rewrite all of that and risk breaking requests that
/// work today. Encoding only what alters the URL's shape cannot change any
/// request that does not already contain those four characters.
///
/// Dot segments are refused rather than encoded, because encoding does not
/// stop them: WHATWG reads `%2e%2e` as a double-dot segment too, so a value
/// of exactly `..` still climbs a level however it is spelled. Encoding the
/// separators is what defeats the multi-level `../../x` case — it collapses
/// to a single segment — and this catches the single-level remainder.
fn path_segment<'a>(name: &str, value: &'a str) -> Result<Cow<'a, str>> {
    // `%2e` is a dot as far as the URL parser is concerned, in either case.
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
/// `reqwest::Error`'s `Display` appends " for url (…)", and the access token
/// travels in the query string — so the default rendering puts a live token
/// into an error message, which under `--output json` is a field that agents,
/// CI logs and pasted issue reports all capture verbatim. The `--debug` path
/// above already redacts the token for exactly this reason.
///
/// Stripping the URL costs the only detail reqwest's outermost layer carried,
/// so the source chain is walked to recover what actually went wrong —
/// otherwise every proxy, DNS, TLS and timeout failure reads "error sending
/// request" and nothing more.
///
/// A timeout is then told apart from the rest and given its own code, because
/// it is the one of them a caller can do something about from here: the
/// budget it ran out of is one `--timeout` moves. Sending that reader to
/// check their proxy, which is what the generic advice does, points away from
/// the answer.
///
/// Shared with `auth`'s `--verify`, which reaches an endpoint that takes the
/// token as a query parameter too. One redaction, not two.
pub(crate) fn transport_failure(context: &str, err: reqwest::Error) -> CliError {
    // Asked before `without_url`, which builds a new error out of this one.
    let ran_out_of_time = err.is_timeout();
    let err = err.without_url();
    let mut message = format!("{context}: {err}");

    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }

    if ran_out_of_time {
        // Its own code, not a variant of the message: this is the one
        // transport failure a script has a sensible thing to do about, which
        // is to retry it or to raise the budget. Reading prose to find that
        // out is not a contract.
        return CliError::new("request_timed_out", message).with_remedy(remedy::for_timeout());
    }

    CliError::new("request_failed", message).with_remedy(remedy::for_transport())
}

/// Points a rejected argument at `--schema`, which describes what the
/// command would have accepted.
///
/// Only for the codes it answers, since this sees every failure `execute`
/// produces: an unreadable `--file` path is a filesystem problem and a 404
/// is not an argument problem at all, so offering a schema for either would
/// be advice that does not apply.
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
/// name a command that exists — `accounts list-tokens` gets nothing, because
/// the Tokens API has no way to fetch one token by id, and inventing a
/// plausible-looking suggestion is worse than staying quiet.
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

/// The file extension to suggest for a response we are refusing to print.
///
/// Guessing from the content type rather than always saying `.png`: telling
/// someone to redirect a glyph range into `out.png` is advice that produces
/// a mislabelled file, and it reads as though the command misunderstood what
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

/// Writes raw bytes to stdout, refusing to do so when that is a terminal —
/// `curl`'s behavior, and for the same reason: a few hundred KB of PNG will
/// otherwise scramble the user's shell.
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

    /// The `CliError` inside a refusal, so a test can name the code the
    /// caller would see rather than match on prose.
    fn refusal(err: anyhow::Error) -> CliError {
        err.downcast::<CliError>().expect("refused with a CliError")
    }

    fn body(content_types: &[&str], multipart_field: Option<&str>) -> RequestBody {
        RequestBody {
            // Nothing here reads it: `resolve_body_source` decides from the
            // flags it was given, not from whether the spec insists on one.
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
            // Parameters and casing must not change the answer, and an
            // unknown type still has to name something.
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
        // `{highRes}` and `{format}` are the segment's optional tail: empty
        // means the default, not a segment to remove.
        let template = "/styles/v1/user/s/tiles/512/1/2/3{highRes}{format}";
        assert_eq!(
            substitute_path_param(template, "highRes", "", false),
            "/styles/v1/user/s/tiles/512/1/2/3{format}"
        );
    }

    #[test]
    fn a_required_segment_is_left_empty_rather_than_dropped() {
        // Dropping it would send a URL that means something else. The empty
        // segment reaches the API and it says what is wrong.
        let template = "/v4/{tilesets}/1/2/3.png";
        assert_eq!(
            substitute_path_param(template, "tilesets", "", true),
            "/v4//1/2/3.png"
        );
    }

    /// `starFile`'s body is the word `true`, under `text/plain`. It goes
    /// out as typed: parsing it as JSON and re-encoding would be a
    /// round trip through a format the endpoint rejects.
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

    /// The JSON operations are untouched by that.
    #[test]
    fn a_json_body_still_goes_through_data() {
        let json = body(&["application/json"], None);
        assert_eq!(
            resolve_body_source(&json, Some("{}"), &[]).unwrap(),
            BodySource::Json("{}")
        );
    }

    /// A 204 says only that it worked. The method is what makes the line
    /// specific, and an empty body must never read as an empty result.
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

    /// The session endpoints answer 200 with nothing in the body. They change
    /// no state, so the line must not claim they did.
    #[test]
    fn an_empty_get_claims_nothing() {
        assert_eq!(empty_success_line("GET", None), "No content.");
        assert_eq!(empty_success_line("GET", Some("ignored")), "No content.");
    }

    /// The spec's media type has to reach the wire verbatim. Sending SVG as
    /// `application/json` is exactly the bug that made `upload-sprite-image`
    /// unusable, and it looked like a working command the whole time.
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

    /// The field name comes from the spec, so a spec that renames it does not
    /// need this code changed.
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

    /// An operation that takes a body but was given nothing still sends the
    /// request: whether the body was required is the API's answer to give.
    #[test]
    fn no_flag_at_all_sends_an_empty_body() {
        let json = body(&["application/json"], None);
        assert_eq!(
            resolve_body_source(&json, None, &[]).unwrap(),
            BodySource::Empty
        );
    }

    /// Both flags set one body between them, so taking either silently would
    /// discard what the caller asked for.
    #[test]
    fn data_and_file_together_are_refused() {
        let both = body(&["application/octet-stream", "application/json"], None);
        let err = refusal(resolve_body_source(&both, Some("{}"), &["chunk.bin"]).unwrap_err());
        assert_eq!(err.code, "conflicting_body");
        assert!(err.message.contains("--data"), "{}", err.message);
        assert!(err.message.contains("--file"), "{}", err.message);
    }

    /// `--file` is never offered on a JSON-only operation, but the resolver
    /// must not depend on clap having enforced that.
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

    /// `initUpload` declares octet-stream *and* JSON. The first non-JSON type
    /// is what `--file` means; `--data` keeps the JSON half.
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
    /// directories in front of it must not travel with it.
    #[test]
    fn a_part_is_named_by_the_file_not_its_path() {
        assert_eq!(
            file_name_of("/tmp/sprite/zz-clitest-1.svg"),
            "zz-clitest-1.svg"
        );
        assert_eq!(file_name_of("zz-clitest-1.svg"), "zz-clitest-1.svg");
        assert_eq!(file_name_of("./a/b/c.svg"), "c.svg");
    }

    /// The spec calls every part `format: binary` and stops there, so the
    /// extension is the only evidence — and `batchUploadSprite` rejects a
    /// part that does not claim to be SVG.
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

    /// The token travels in the query string, so the one function that
    /// renders this URL is the one thing standing between a live credential
    /// and `--debug`'s stderr or a dry run's stdout.
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

    /// An unauthenticated call has no query at all, and a URL ending in `?`
    /// is not the request that would be sent.
    #[test]
    fn a_url_with_no_query_keeps_no_question_mark() {
        assert_eq!(
            redacted_url("https://api.mapbox.com/styles/v1/me", &[]),
            "https://api.mapbox.com/styles/v1/me"
        );
    }

    /// The dry run's whole promise is that what it accepts, the real call
    /// would send. A body it declined to parse breaks that in the direction
    /// that costs the most: a `--dry-run` that passes and a `create` that
    /// then fails on the same argument.
    #[test]
    fn a_dry_run_rejects_the_json_that_sending_would_reject() {
        let err = describe_body(&BodySource::Json("{oops")).expect_err("invalid JSON is refused");
        assert_eq!(refusal(err).code, "invalid_data");
    }

    /// And a `--file` that is not there. Reading rather than stat-ing is what
    /// makes this catch an unreadable file too, which a metadata check would
    /// wave through.
    #[test]
    fn a_dry_run_rejects_a_file_that_cannot_be_read() {
        let err = describe_body(&BodySource::Raw {
            path: "/nonexistent/zz-clitest.svg",
            content_type: "image/svg+xml",
        })
        .expect_err("an unreadable file is refused");
        assert_eq!(refusal(err).code, "invalid_file");
    }

    /// An operation whose body the caller left out has nothing to describe,
    /// and a "Body:" line saying so would be noise on every `delete`.
    #[test]
    fn a_body_that_was_never_given_is_not_described() {
        assert!(describe_body(&BodySource::Empty)
            .expect("an absent body is not an error")
            .is_none());
    }

    /// `batchUploadSprite` takes each icon's name from its part's filename,
    /// so the plan has to name the icons the upload would create — the path
    /// alone leaves the reader to work out that `icons/foo.svg` becomes
    /// `foo.svg`.
    #[test]
    fn a_multipart_plan_names_each_part() {
        // `CARGO_TARGET_TMPDIR` is an integration-test variable and does not
        // exist here; the pid keeps two concurrent runs on one CI box apart.
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

    /// A budget that runs out is reported as that, not as a network to go and
    /// check.
    ///
    /// Built from a real `reqwest` timeout rather than a hand-made error,
    /// because what is being pinned is that `is_timeout()` still answers true
    /// after the whole `transport_failure` path has taken the URL out of the
    /// error — the token rides in that URL, so the stripping is not optional
    /// and the classification has to survive it.
    #[test]
    fn a_budget_that_runs_out_says_so_rather_than_blaming_the_network() {
        // Accepts and never answers, and is held open: dropped, the port
        // closes and the client gets a refusal instead of a silence.
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

    /// Which requests get the longer budget, and — the half that is easy to
    /// get wrong — which do not.
    ///
    /// A `--data` body is bounded by what a command line can carry, so
    /// putting fifteen minutes in front of a `create-style` would only mean
    /// waiting a quarter of an hour to be told the API is down.
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

    /// A body read from `@path` or `@-` is indistinguishable from a typed one
    /// by the time it reaches `BodySource::Json`, and nothing bounds its size.
    /// Given the sixty-second budget, a large one would fail on a timeout that
    /// described the network rather than the choice of flag.
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

    /// The trailing newline a text editor leaves is *not* stripped. It is
    /// insignificant to every JSON parser, and trimming a body the caller
    /// supplied would be this CLI quietly editing what it was asked to send.
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

    /// A JSON body has to be UTF-8, so this is a check rather than a
    /// convenience — the alternative is a lossy conversion the API rejects for
    /// reasons that name nothing the caller did.
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

    /// The mistake is almost always a pipe that produced nothing, and the
    /// symptom without this is "EOF while parsing a value", which names the
    /// parser rather than the pipe.
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

    /// A body that merely *contains* an `@` is not a path. Only the first
    /// character decides, which is what makes `--data '{"a":"b@c"}'` safe.
    #[test]
    fn an_at_sign_inside_the_body_is_not_a_path() {
        let resolved = resolve_data(r#"{"email":"a@b.example"}"#).expect("a literal body");
        assert!(!resolved.streamed);
        assert_eq!(resolved.body, r#"{"email":"a@b.example"}"#);
    }

    /// A scratch directory that cleans itself up, so these tests leave
    /// nothing behind and cannot collide with each other.
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

    /// A parameter the API sent back but this command does not declare has no
    /// flag to name, so it is left out rather than invented.
    #[test]
    fn an_undeclared_query_parameter_is_not_named() {
        let declared = [param("start")];
        let next = "https://api.mapbox.com/a?start=7&fresh=true";

        let tip = NextPage::of(&declared, next).tip();
        assert!(tip.contains("--start 7"), "{tip}");
        assert!(!tip.contains("fresh"), "{tip}");
    }

    /// **The security property of this whole path.**
    ///
    /// The access token rides in the query string, so the URL the API echoes
    /// back in `Link` contains a live token. It is excluded by construction —
    /// `dispatch` adds it to the query directly rather than declaring it in
    /// `op.query_params`, and only declared parameters become flags — but
    /// "by construction" is worth a test, because the cost of being wrong is
    /// printing a credential to a terminal and into whatever captured it.
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

    /// Even if someone later declares a parameter by that name, which is the
    /// way the guarantee above could be undone from a spec rather than from
    /// this file.
    #[test]
    fn a_declared_parameter_named_access_token_is_still_withheld() {
        let declared = [param(ACCESS_TOKEN), param("start")];
        let next = "https://api.mapbox.com/a?access_token=pk.secret&start=3";

        let tip = NextPage::of(&declared, next).tip();
        assert!(!tip.contains("pk.secret"), "{tip}");
        assert!(tip.contains("--start 3"), "{tip}");
    }

    /// The values are decoded, because the CLI re-encodes whatever it is
    /// given: handing back `%2B` would round-trip to `%252B` and fetch the
    /// wrong page.
    #[test]
    fn the_page_tip_decodes_percent_escapes() {
        let declared = [param("start")];
        let next = "https://api.mapbox.com/a?start=a%2Bb";

        let tip = NextPage::of(&declared, next).tip();
        assert!(tip.contains("--start a+b"), "{tip}");
    }

    /// A value with a space in it would silently become two arguments if the
    /// tip were pasted unquoted.
    #[test]
    fn a_value_needing_a_shell_quote_gets_one() {
        assert_eq!(shell_value("cjk2ab"), "cjk2ab");
        assert_eq!(shell_value("2026-09-01"), "2026-09-01");
        assert_eq!(shell_value("a b"), "'a b'");
        assert_eq!(shell_value(""), "''");
        assert_eq!(shell_value("it's"), r"'it'\''s'");
    }

    /// An operation the API pages but whose spec declares no paging
    /// parameter. The result is incomplete either way, so saying so beats
    /// saying nothing — but it must not claim a flag that does not exist.
    #[test]
    fn no_declared_paging_parameter_still_says_the_result_is_partial() {
        let tip = NextPage::of(&[param("unrelated")], "https://api.mapbox.com/a?start=3").tip();
        assert!(tip.contains("More results exist"), "{tip}");
        assert!(!tip.contains("--"), "{tip}");
    }

    /// An unparseable `Link` target costs nothing: the request succeeded, and
    /// a tip is not worth turning that into a failure.
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
    /// No Mapbox endpoint reachable from here sends `x-request-id` — styles,
    /// tokens, fonts and geocoding v6 were all checked, on success and on a
    /// 404. Every one of them sends `x-amz-cf-id`, because the API is fronted
    /// by CloudFront. Looking for the conventional name alone would have made
    /// this feature inert, which is what this test exists to stop happening
    /// again.
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

    /// A service that sends its own id means it more specifically than the
    /// CDN in front of it does.
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

    /// A header present but blank says nothing, and an empty request id would
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

    /// `--id` searches the page it was handed. On a paginated response
    /// "No row has the id" is true of the page and may be false of the
    /// listing, which is the most misleading form of the truncation this
    /// path exists to stop.
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

    /// The shape mapbox/mcp-server fixed in `directions_tool`: a value spliced
    /// into the path used to append query parameters the caller never asked
    /// for. Verified against `reqwest::Url` at the time — `x?fresh=true` gave
    /// `query = fresh=true&access_token=…`.
    #[test]
    fn a_path_parameter_cannot_inject_a_query_string() {
        let safe = path_segment("style_id", "x?fresh=true").expect("encoded, not refused");
        assert_eq!(safe, "x%3Ffresh=true");
    }

    /// With the caller's token and the command's method attached, this aimed a
    /// `DELETE` at whatever path the value resolved to. The host was never
    /// reachable — `//evil`, `https://evil` and `x@evil` all stay on
    /// `api.mapbox.com` — but the path was.
    #[test]
    fn a_path_parameter_cannot_retarget_the_path() {
        let safe = path_segment("style_id", "../../tokens/v2/victim").expect("encoded");
        assert_eq!(safe, "..%2F..%2Ftokens%2Fv2%2Fvictim");
        assert!(!safe.contains('/'), "one segment, not four: {safe}");
    }

    /// `\` is a path separator too, for a special scheme — WHATWG says so and
    /// `reqwest::Url` agrees: `..\..\tokens` resolved just as `../../tokens`
    /// did. Encoding `/` alone would have left this open.
    #[test]
    fn a_backslash_cannot_retarget_the_path_either() {
        let safe = path_segment("style_id", r"..\..\tokens").expect("encoded");
        assert_eq!(safe, "..%5C..%5Ctokens");
    }

    /// A fragment is not sent to the server, so this silently truncated the
    /// path rather than redirecting it — a request to somewhere the caller
    /// could not see in what they typed.
    #[test]
    fn a_fragment_cannot_truncate_the_path() {
        assert_eq!(
            path_segment("style_id", "x#frag").expect("encoded"),
            "x%23frag"
        );
    }

    /// **Refused, not encoded, and that distinction is the point.** Encoding
    /// does not stop a dot segment: WHATWG reads `%2e%2e` as one too, so a
    /// value of exactly `..` climbs a level however it is spelled. Encoding
    /// the separators handles the multi-level case by collapsing it into one
    /// segment; this handles what is left.
    #[test]
    fn a_dot_segment_is_refused_however_it_is_spelled() {
        for value in ["..", ".", "%2e%2e", "%2E%2E", "%2e", ".%2e", "%2e."] {
            let err = path_segment("style_id", value).expect_err(value);
            assert_eq!(refusal(err).code, "invalid_path_parameter", "{value}");
        }
    }

    /// Encoded dots *and* encoded slashes together, which is the shape that
    /// looks like traversal and is not.
    ///
    /// `%2f` is never decoded into a separator by the URL parser, so this
    /// stays one segment on the wire — unlike encoded dots with *raw* slashes
    /// (`%2e%2e/%2e%2e/x`), which the parser does resolve and which the
    /// encoding above is what stops. It is left alone on purpose: percent-
    /// encoded values are a documented Mapbox feature, not an attack
    /// signature. A custom marker overlay is
    /// `url-https%3A%2F%2Fexample.com%2Fmarker.png(…)`, `geojson(…)` takes
    /// URI-encoded GeoJSON, and the spec says a bbox's brackets "may be sent
    /// literally or percent-encoded as `%5B`". Refusing these would break all
    /// three.
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

    /// **The reason this encodes four characters and not everything outside
    /// RFC 3986's unreserved set.** These values carry punctuation on purpose,
    /// and percent-encoding it would rewrite requests that work today —
    /// `static get-image`'s template alone is
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

    /// An empty optional parameter still drops its whole segment, which
    /// several operations rely on — encoding must not have taken that away.
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
