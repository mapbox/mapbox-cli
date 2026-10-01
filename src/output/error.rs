//! How a failure is shaped and printed.
//!
//! [`CliError`] carries what a caller can branch on — a code, a status, the
//! upstream body, a request id, advice — and [`emit_error`] prints it: one
//! JSON object on stderr under `json`, labeled lines under `text`.

use serde_json::{json, Value};

use super::{encode, Mode};

/// Code carried by an error that nothing has classified further.
pub const GENERIC_CODE: &str = "error";

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
    let code = cli.map_or(GENERIC_CODE, |e| e.code.as_str());
    crate::run_record::set_error(code, &format!("{err:#}"));

    if mode.is_json() {
        let payload = match cli {
            Some(e) => error_payload(e),
            // `{:#}` flattens anyhow's context chain into one line, so a
            // wrapped error keeps the context that explains it.
            None => json!({ "code": GENERIC_CODE, "message": format!("{err:#}") }),
        };
        // Serializing a `json!` object cannot fail; fall back rather than
        // panic while already on the error path.
        let pretty = matches!(mode, Mode::Json { pretty: true });
        let line = encode(&payload, pretty)
            .unwrap_or_else(|_| r#"{"code":"error","message":"unserializable error"}"#.to_string());
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
            eprint_labeled("Next", &e.next_actions);
            eprint_labeled("Docs", &e.docs);
        }
        None => eprintln!("Error: {err:#}"),
    }
}

/// A labeled group of lines on stderr. Nothing for an empty list, so the
/// caller needs no guard.
fn eprint_labeled(label: &str, values: &[String]) {
    for line in labeled_lines(label, values) {
        eprintln!("{line}");
    }
}

/// Labels the first line and aligns the rest under it.
///
/// `Next: mapbox styles list` reads as one thing; a second `Next:` on
/// the line below reads as two unrelated ones. Continuation lines are
/// indented to the label's width instead.
fn labeled_lines(label: &str, values: &[String]) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two commands under one `Next:` have to read as two commands, not as
    /// one wrapped line and not as two unrelated labels.
    #[test]
    fn a_second_labeled_line_is_aligned_under_the_first() {
        let lines = labeled_lines(
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
        assert!(labeled_lines("Next", &[]).is_empty());
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
