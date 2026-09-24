//! What to do about a failure.
//!
//! `CliError` carries what went wrong; this carries what to do about it. The
//! two are separate because they come from different places: the message is
//! the API's, and the remedy is ours. A 404 from the Styles API says "Style
//! not found", and nothing in that answer tells a caller that the ids it
//! could have used come from `mapbox styles list`.
//!
//! Keyed on the HTTP status, not on the command. The status is the one thing
//! all 54 generated commands have in common, and advice written per command
//! would be 54 pieces of prose to keep true as the specs move. Where the
//! command *does* change the answer it is read off the parsed spec — the
//! listing a detail operation's ids come from, the `--schema` call that
//! prints what this operation accepts — so a suggestion can only ever name a
//! command that exists.

use clap::ArgMatches;

use crate::spec::{Operation, ACCOUNT_PLACEHOLDERS};

/// Advice attached to a failure: one line to read, commands to run, pages to
/// consult.
///
/// `next_actions` holds shell commands and nothing else — no prose, no
/// explanation of why one is there. What a caller does with the field is run
/// what is in it, and a list that has to be parsed first is just a worse
/// `fix`. The explanation belongs in `fix`, which is where a person reads it.
#[derive(Debug, Default)]
pub struct Remedy {
    pub fix: Option<String>,
    pub next_actions: Vec<String>,
    pub docs: Vec<String>,
}

impl Remedy {
    pub fn with_doc(mut self, url: Option<&str>) -> Self {
        if let Some(url) = url {
            let url = url.to_string();
            if !self.docs.contains(&url) {
                self.docs.push(url);
            }
        }
        self
    }

    pub fn with_fix(mut self, fix: &str) -> Self {
        self.fix = Some(fix.to_string());
        self
    }

    /// Takes an `Option` because most callers are answering a question
    /// that may have no answer — the listing an operation's ids come from
    /// exists for some commands and not others.
    pub fn with_action(mut self, command: Option<String>) -> Self {
        if let Some(command) = command {
            self.next_actions.push(command);
        }
        self
    }
}

/// The documentation page for each service the CLI exposes.
///
/// Hand-maintained, because there is nothing to derive it from: not one of
/// the eleven specs bundled from `openapi-specs` declares `externalDocs`
/// (checked there on 2026-09-02), and neither does the hand-authored
/// `search` spec that makes up the twelfth service.
/// `every_service_has_a_documentation_page` in `main.rs` fails the build if
/// a service is ever added without an entry, which is the only thing keeping
/// this list complete.
// `rasterarrays` and `tilequery` are absent on purpose, not by omission:
// #116 folded their one operation each into `tilesets` (see
// command-config.yaml), so neither is a service `bundled_specs()` ever
// returns any more — an entry here for either would fail
// `every_documentation_page_belongs_to_a_service`.
const SERVICE_DOCS: &[(&str, &str)] = &[
    ("accounts", TOKENS_DOC),
    (
        "directions",
        "https://docs.mapbox.com/api/navigation/directions/",
    ),
    ("feedback", "https://docs.mapbox.com/api/feedback/"),
    ("fonts", "https://docs.mapbox.com/api/maps/fonts/"),
    (
        "geocoder",
        "https://docs.mapbox.com/api/search/geocoding-v6/",
    ),
    (
        "isochrone",
        "https://docs.mapbox.com/api/navigation/isochrone/",
    ),
    (
        "map-matching",
        "https://docs.mapbox.com/api/navigation/map-matching/",
    ),
    ("matrix", "https://docs.mapbox.com/api/navigation/matrix/"),
    (
        "optimization",
        "https://docs.mapbox.com/api/navigation/optimization/",
    ),
    ("search", "https://docs.mapbox.com/api/search/search-box/"),
    // Static Images and Static Tiles merged into one `static` command group
    // (#116); neither upstream page covers both, so this points at Static
    // Images, the one `getStaticImage` (this group's more-used half) documents.
    ("static", "https://docs.mapbox.com/api/maps/static-images/"),
    // The sprite endpoints are part of the Styles API upstream — the command
    // group is this CLI's own split, so both point at the same page.
    ("sprites", "https://docs.mapbox.com/api/maps/styles/"),
    ("styles", "https://docs.mapbox.com/api/maps/styles/"),
    // The index rather than one of the pages under it: this service now
    // holds a raster-tile, an MRT-tile, a tilequery and a vector-tile
    // command (#116), documented separately upstream, and picking any one
    // of those pages would be wrong for the other three.
    ("tilesets", "https://docs.mapbox.com/api/maps/"),
];

/// Where tokens, their scopes and their expiry are documented — the page
/// behind every failure that is about the credential rather than about the
/// request. Public because `auth` owns the advice for a 401.
pub const TOKENS_DOC: &str = "https://docs.mapbox.com/api/accounts/tokens/";

/// Rate limits and the headers that report them live here.
const API_OVERVIEW_DOC: &str = "https://docs.mapbox.com/api/overview/";

/// Whether the platform itself is degraded — the question a 5xx raises and
/// the CLI cannot answer.
const STATUS_PAGE: &str = "https://status.mapbox.com/";

/// Every service the table has a page for, so a stale entry — a service
/// renamed or dropped from `spec::effective_spec_entries()` — can be caught
/// as well as a missing one. Exists for that test and nothing else.
#[cfg(test)]
pub fn documented_services() -> impl Iterator<Item = &'static str> {
    SERVICE_DOCS.iter().map(|(name, _)| *name)
}

/// The documentation page for a service, by the name
/// `spec::effective_spec_entries()` gives it (a custom-only service needs a
/// row here too).
pub fn docs_for_service(service: &str) -> Option<&'static str> {
    SERVICE_DOCS
        .iter()
        .find(|(name, _)| *name == service)
        .map(|(_, url)| *url)
}

/// The remedy for a non-2xx answer to a generated command.
///
/// `matches` and `username` are what the failed invocation actually supplied:
/// a suggestion has to carry them, or it is a command line that will not run.
pub fn for_http(
    status: u16,
    op: &Operation,
    matches: &ArgMatches,
    username: Option<&str>,
) -> Remedy {
    let remedy = Remedy::default().with_doc(docs_for_service(&op.service));

    match status {
        // 401 is the one status whose answer depends on where the token came
        // from, and only `auth` knows that. It fills the fix in through
        // `with_auth_fix`; this stays out of the way and contributes the page.
        401 => remedy.with_doc(Some(TOKENS_DOC)),
        // Rejected before anything happened, which makes it an argument
        // problem — and the CLI can print what the arguments are.
        400 | 422 => remedy
            .with_fix(
                "The API rejected the request without acting on it. `--schema` prints \
                 every argument this command accepts, the values the spec allows, and \
                 where each one lands in the request.",
            )
            .with_action(Some(schema_command(op))),
        403 => remedy
            .with_fix(
                "The token was accepted but is not allowed to do this: either it lacks \
                 the scope the operation needs, or it belongs to an account that does \
                 not own what the request names.",
            )
            .with_action(Some("mapbox auth whoami".to_string()))
            .with_doc(Some(TOKENS_DOC)),
        // `whoami` is here on every 404, not only on an account-scoped path:
        // the Mapbox APIs answer a request the token is not allowed to make
        // with 404 rather than 403 — `accounts list-tokens` with a `pk`
        // token does exactly that — so "which account am I?" is always a
        // live question here, and not only when the path names one.
        404 => remedy
            .with_fix(
                "Nothing exists at that path. The id may be misspelled, or it may \
                 belong to an account other than the one this token is for.",
            )
            .with_action(listing_command(op, matches, username))
            .with_action(Some("mapbox auth whoami".to_string())),
        409 => remedy.with_fix(
            "Something with that name or id already exists. Delete it first, or send \
             the change as an update instead of a create.",
        ),
        429 => remedy
            .with_fix(
                "Rate limited. Back off and retry — the response headers name the \
                 limit that was hit and when it resets.",
            )
            .with_doc(Some(API_OVERVIEW_DOC)),
        // The request may well have been fine. Retrying is the first move,
        // and the status page is the second — the CLI cannot tell a caller
        // whether the platform is degraded.
        500..=599 => remedy
            .with_fix(
                "The failure is the API's, not the request's. Retry; if it persists, \
                 the status page says whether the service is degraded.",
            )
            .with_doc(Some(STATUS_PAGE)),
        // Every other status gets the page and no invented advice. A wrong
        // fix costs more than a missing one.
        _ => remedy,
    }
}

/// The remedy for a request that never reached the API.
///
/// Shared with `auth --verify`, which fails the same way through the same
/// `transport_failure`. Deliberately says nothing about the token: nothing
/// was answered, so nothing was rejected.
pub fn for_transport() -> Remedy {
    Remedy::default()
        .with_fix(
            "Nothing was reached, so nothing was rejected — this is the network, not \
             the request. Check connectivity and any proxy in the environment \
             (HTTPS_PROXY, ALL_PROXY, NO_PROXY). A SOCKS proxy is not supported: \
             the message above says `unsupported scheme socks5` when that is the \
             cause.",
        )
        .with_doc(Some(STATUS_PAGE))
}

/// The remedy for a request that ran out of time.
///
/// Split from [`for_transport`] because the advice above is wrong here in a
/// way that costs the reader the answer. A timeout is not evidence that
/// nothing was reached — the connection may have opened, the upload may have
/// been most of the way through — and someone sent to check their proxy over
/// a budget that a flag would have fixed is looking in the wrong place. The
/// status page still belongs: a slow API is the other thing this means.
pub fn for_timeout() -> Remedy {
    Remedy::default()
        .with_fix(
            "The request ran out of its time budget. That is a slow connection, a large \
             upload, or an API not answering — not a rejected request, since nothing was \
             answered. Raise it with `--timeout <seconds>` (or MAPBOX_TIMEOUT), which \
             defaults to 60 seconds and to 900 for a body read from --file.",
        )
        .with_doc(Some(STATUS_PAGE))
}

/// `mapbox <service> <command> --schema`, which describes an operation
/// without running it or spending a token.
pub fn schema_command(op: &Operation) -> String {
    format!("mapbox {} --schema", op.command())
}

/// The listing a detail operation's ids come from, as a line that will run —
/// `mapbox styles list --username someone` for a failed `mapbox styles get`.
///
/// The listing is scoped by the same path as the operation that failed, less
/// its last segment, so it can need path parameters of its own:
/// `/styles/v1/{username}/{style_id}/sprite` still wants a style. Those
/// values are taken from the invocation that just failed, in the order the
/// listing's path puts them, because that is the only place they exist.
///
/// `None` when any of them cannot be recovered. A suggestion that does not
/// parse is worse than no suggestion: it costs the caller a second failure
/// to find out this one was guessing.
fn listing_command(op: &Operation, matches: &ArgMatches, username: Option<&str>) -> Option<String> {
    let listing = op.listing.as_ref()?;
    let mut command = format!("mapbox {}", listing.command);

    for placeholder in placeholders_of(&listing.path_template) {
        // Filled from `--username` below, the same way the request itself
        // fills it, rather than as a positional.
        if ACCOUNT_PLACEHOLDERS.contains(&placeholder) {
            continue;
        }
        let param = op.path_params.iter().find(|p| p.name == placeholder)?;
        // `try_get_one`, not `get_one`: the latter panics on an argument that
        // was never registered, and a spec can put a placeholder in a path
        // without declaring a parameter for it. Same reason `execute` reads
        // `--data` and `--file` that way.
        let value = matches
            .try_get_one::<String>(&param.arg_name)
            .ok()
            .flatten()?;
        command.push(' ');
        command.push_str(&shell_quoted(value));
    }

    // The account may have been typed rather than stored, so spell out the
    // one the failed request used. Otherwise a caller who passed
    // `--username` would be handed a command that lists someone else's.
    if let Some(username) = username.filter(|_| takes_an_account(op)) {
        command.push_str(&format!(" --username {}", shell_quoted(username)));
    }
    Some(command)
}

/// The `{placeholder}` names in a path, in the order the path puts them —
/// which is the order clap takes the positionals for them.
fn placeholders_of(path: &str) -> impl Iterator<Item = &str> {
    path.split('{')
        .skip(1)
        .filter_map(|rest| rest.split('}').next())
}

/// A value as a shell word.
///
/// `next_actions` is a list of command lines, so a value with a space in it
/// has to survive being one: a style whose id or folder name is two words
/// would otherwise silently become two arguments.
fn shell_quoted(value: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "._-/:,@+=".contains(c);
    if !value.is_empty() && value.chars().all(safe) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Whether this operation's path is scoped to an account.
fn takes_an_account(op: &Operation) -> bool {
    ACCOUNT_PLACEHOLDERS
        .iter()
        .any(|name| op.path_template.contains(&format!("{{{name}}}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{parse_spec, ServiceSpec};
    use clap::{Arg, Command};

    /// The positional values an invocation supplied, as clap would have
    /// parsed them. Built here rather than off `build_app` so a fixture spec
    /// is all a test needs.
    fn supplied(values: &[(&str, &str)]) -> ArgMatches {
        let mut command = Command::new("fixture");
        let mut argv = vec!["fixture".to_string()];
        for (arg_name, value) in values {
            command = command.arg(Arg::new(arg_name.to_string()).required(false));
            argv.push(value.to_string());
        }
        command.try_get_matches_from(argv).expect("fixture parses")
    }

    /// Nothing supplied — the case a suggestion must not invent values for.
    fn nothing() -> ArgMatches {
        supplied(&[])
    }

    /// A listing and the operation that shows one of its items, which is the
    /// pairing every 404 suggestion is built from. Named `styles` because the
    /// service name is what `docs_for_service` looks up.
    fn styles() -> ServiceSpec {
        parse_spec(
            "styles",
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /styles/v1/{username}:
    get: { operationId: listStyles, summary: List }
  /styles/v1/{username}/{style_id}:
    get: { operationId: getStyle, summary: One }
"#,
        )
        .expect("fixture parses")
    }

    /// A listing that is itself scoped to a style — the shape that broke the
    /// first version of `listing_command`. Its path parameters are declared,
    /// the way the real spec declares them.
    fn sprites() -> ServiceSpec {
        parse_spec(
            "styles",
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /styles/v1/{username}/{style_id}/sprite:
    get:
      operationId: getSpriteJson
      summary: List
      parameters:
        - { name: style_id, in: path, required: true, schema: { type: string } }
  /styles/v1/{username}/{style_id}/sprite/{icon_name}:
    get:
      operationId: getSpriteImage
      summary: One
      parameters:
        - { name: style_id, in: path, required: true, schema: { type: string } }
        - { name: icon_name, in: path, required: true, schema: { type: string } }
"#,
        )
        .expect("fixture parses")
    }

    fn operation<'a>(svc: &'a ServiceSpec, name: &str) -> &'a Operation {
        svc.operations
            .iter()
            .find(|op| op.command_name() == name)
            .expect("operation built")
    }

    /// The point of the whole module: a 404 is usually an id that does not
    /// exist, and the ids that do come from a command the caller can run.
    #[test]
    fn a_404_points_at_the_listing_the_ids_come_from() {
        let svc = styles();
        let remedy = for_http(
            404,
            operation(&svc, "get-style"),
            &nothing(),
            Some("someone"),
        );

        assert_eq!(
            remedy.next_actions,
            [
                "mapbox styles list-styles --username someone",
                "mapbox auth whoami"
            ],
            "the listing comes first: it is the one that answers `which ids exist`"
        );
        assert_eq!(remedy.docs, ["https://docs.mapbox.com/api/maps/styles/"]);
        assert!(remedy.fix.is_some());
    }

    /// The account has to be the one the failed request used. A caller who
    /// passed `--username` would otherwise be handed a command that lists
    /// somebody else's styles and reports them as the ids that exist.
    #[test]
    fn the_suggested_listing_names_the_account_that_failed() {
        let svc = styles();
        let for_someone = for_http(
            404,
            operation(&svc, "get-style"),
            &nothing(),
            Some("someone"),
        );
        assert!(for_someone.next_actions[0].ends_with("--username someone"));

        // Nothing resolved an account, so there is nothing to name — and
        // guessing one would be worse than leaving the flag off.
        let anonymous = for_http(404, operation(&svc, "get-style"), &nothing(), None);
        assert_eq!(anonymous.next_actions[0], "mapbox styles list-styles");
    }

    /// A listing can need path parameters of its own: the sprite listing is
    /// scoped to a style, and a suggestion that dropped it named a command
    /// clap would reject. Those values can only come from the invocation
    /// that just failed.
    #[test]
    fn the_suggested_listing_carries_the_path_it_shares() {
        let svc = sprites();

        let supplied = supplied(&[("style-id", "my-style"), ("icon-name", "pin")]);
        let remedy = for_http(
            404,
            operation(&svc, "get-sprite-image"),
            &supplied,
            Some("someone"),
        );

        assert_eq!(
            remedy.next_actions[0], "mapbox styles get-sprite-json my-style --username someone",
            "the style the sprite belongs to is part of the listing's own path"
        );
    }

    /// And when a shared parameter cannot be recovered from the invocation —
    /// nothing supplied for it, or a spec that never declared it — the
    /// suggestion is dropped rather than emitted incomplete.
    #[test]
    fn a_listing_whose_path_cannot_be_filled_is_not_suggested() {
        let svc = sprites();

        let remedy = for_http(
            404,
            operation(&svc, "get-sprite-image"),
            &nothing(),
            Some("someone"),
        );

        assert_eq!(
            remedy.next_actions,
            ["mapbox auth whoami"],
            "an incomplete listing command is worse than none"
        );
    }

    /// A value with a space in it has to survive being one shell word.
    #[test]
    fn a_value_that_needs_quoting_gets_it() {
        assert_eq!(shell_quoted("my-style"), "my-style");
        assert_eq!(shell_quoted("two words"), "'two words'");
        assert_eq!(shell_quoted("it's"), r"'it'\''s'");
        assert_eq!(shell_quoted(""), "''");
    }

    /// An operation whose path is not account-scoped must not be handed a
    /// `--username` it has no use for.
    #[test]
    fn an_account_free_operation_is_not_given_an_account() {
        let svc = parse_spec(
            "styles",
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /things/v1:
    get: { operationId: listThings, summary: List }
  /things/v1/{thing_id}:
    get: { operationId: getThing, summary: One }
"#,
        )
        .expect("fixture parses");

        let remedy = for_http(
            404,
            operation(&svc, "get-thing"),
            &nothing(),
            Some("someone"),
        );
        assert_eq!(remedy.next_actions[0], "mapbox styles list-things");
    }

    /// A listing that 404s has no listing of its own to be pointed at, and
    /// inventing one would name a command that does not exist.
    #[test]
    fn a_listing_is_not_pointed_at_itself() {
        let svc = styles();
        let remedy = for_http(
            404,
            operation(&svc, "list-styles"),
            &nothing(),
            Some("someone"),
        );
        assert_eq!(remedy.next_actions, ["mapbox auth whoami"]);
    }

    /// A rejected request is an argument problem, and the CLI can print what
    /// the arguments are without spending another round trip.
    #[test]
    fn a_rejected_request_offers_the_schema() {
        let svc = styles();
        for status in [400, 422] {
            let remedy = for_http(status, operation(&svc, "get-style"), &nothing(), None);
            assert_eq!(
                remedy.next_actions,
                ["mapbox styles get-style --schema"],
                "HTTP {status}"
            );
        }
    }

    /// 401 is the one status whose answer depends on where the token came
    /// from, which only `auth` knows. This must leave room for it rather
    /// than filling the field with a guess.
    #[test]
    fn a_401_leaves_the_fix_to_auth_but_still_carries_the_page() {
        let svc = styles();
        let remedy = for_http(401, operation(&svc, "get-style"), &nothing(), None);

        assert!(remedy.fix.is_none(), "auth writes this one");
        assert!(remedy.next_actions.is_empty());
        assert!(remedy.docs.contains(&TOKENS_DOC.to_string()));
    }

    /// A 403 is not a 401: the token was accepted, so "sign in again" is the
    /// wrong answer and the scope or the account is the right question.
    #[test]
    fn a_403_asks_about_the_scope_and_the_account() {
        let svc = styles();
        let remedy = for_http(403, operation(&svc, "get-style"), &nothing(), None);

        let fix = remedy.fix.expect("a 403 has an explanation");
        assert!(fix.contains("scope"), "{fix}");
        assert!(fix.contains("account"), "{fix}");
        assert_eq!(remedy.next_actions, ["mapbox auth whoami"]);
    }

    /// A status nobody has written advice for still gets the page. Inventing
    /// a fix for it would cost more than saying nothing.
    #[test]
    fn a_status_with_no_advice_still_gets_the_page() {
        let svc = styles();
        let remedy = for_http(402, operation(&svc, "get-style"), &nothing(), None);

        assert!(remedy.fix.is_none());
        assert!(remedy.next_actions.is_empty());
        assert_eq!(remedy.docs, ["https://docs.mapbox.com/api/maps/styles/"]);
    }

    /// Nothing was answered, so nothing was rejected — the advice must not
    /// send the caller looking at their token.
    #[test]
    fn a_transport_failure_is_about_the_network_and_not_the_token() {
        let remedy = for_transport();
        let fix = remedy.fix.expect("an explanation");

        assert!(fix.contains("network"), "{fix}");
        assert!(!fix.to_lowercase().contains("token"), "{fix}");
        assert_eq!(remedy.docs, [STATUS_PAGE]);
    }

    /// A 5xx is the API's failure, and retrying is the first move.
    #[test]
    fn a_server_failure_says_so_and_names_the_status_page() {
        let svc = styles();
        for status in [500, 502, 503] {
            let remedy = for_http(status, operation(&svc, "get-style"), &nothing(), None);
            let fix = remedy.fix.expect("an explanation");
            assert!(fix.contains("Retry"), "HTTP {status}: {fix}");
            assert!(remedy.docs.contains(&STATUS_PAGE.to_string()));
        }
    }

    /// Two layers write to one error, and the second must not silently drop
    /// what the first attached.
    #[test]
    fn a_page_is_never_listed_twice() {
        let svc = styles();
        // `accounts` is the tokens service, so its own page *is* TOKENS_DOC —
        // the case where the service link and the credential link collide.
        let accounts = parse_spec(
            "accounts",
            r#"
openapi: 3.0.0
info: { title: T }
paths:
  /tokens/v2/{username}:
    get: { operationId: listTokens, summary: List }
"#,
        )
        .expect("fixture parses");

        let remedy = for_http(403, operation(&accounts, "list-tokens"), &nothing(), None);
        assert_eq!(remedy.docs, [TOKENS_DOC]);

        // And the ordinary case still carries both.
        let remedy = for_http(403, operation(&svc, "get-style"), &nothing(), None);
        assert_eq!(
            remedy.docs,
            ["https://docs.mapbox.com/api/maps/styles/", TOKENS_DOC]
        );
    }
}
