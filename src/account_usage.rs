//! `mapbox usage` — account/token usage by product and day.
//!
//! Calls the Statistics API (`GET /statistics/v1`); the token needs the
//! `statistics:read` scope. `mapbox auth login` requests it by default; a
//! token from before that scope was added won't carry it until logged in
//! again.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::Value;

use crate::executor;
use crate::http;
use crate::output::{self, CliError, Mode};
use crate::remedy::Remedy;
use crate::spec::{parse_spec, Operation};
// `--help` points at the tracker rather than at one issue in it: issue
// numbers do not survive a repository move, and a dead number in help text
// printed by every shipped binary is worse than no number at all.
use crate::REPO_URL;

pub const COMMAND: &str = "usage";

/// Where the request goes and what it may carry — base URL, path, and the
/// query parameter names — read from a spec this repo writes and versions
/// itself, since openapi-specs publishes none for this API.
///
/// Deliberately not wired into `CUSTOM_SPEC_ENTRIES` — see `src/spec.rs`,
/// `statistics` is the one exception among the entries there: the generic
/// pipeline registers a subcommand for every entry unconditionally and only
/// in a two-level `<service> <operation>` shape, so it can express neither
/// this command's feature-flag gate nor its single-level surface. Only the
/// request shape comes from the spec; the flags below stay hand-declared for
/// their help text.
pub(crate) const SPEC: &str = include_str!("../custom-openapi/statistics/openapi/statistics.yaml");

const ACCESS_TOKEN: &str = "access_token";
const REDACTED: &str = "<redacted>";

const TOKEN_ID_ARG: &str = "token-id";
const PERIOD_START_ARG: &str = "period-start";
const PERIOD_END_ARG: &str = "period-end";
const PRODUCT_ARG: &str = "product";
const DAILY_ARG: &str = "daily";

/// The one operation [`SPEC`] describes, parsed once.
///
/// Panics if the checked-in YAML stops parsing or stops describing exactly
/// one operation. Unlike `generate_skills`'s and `schema`'s same-shaped
/// `expect`s, which only run under `#[cfg(test)]`, this one is reachable in
/// production — by running `mapbox usage`. It's still the right call: the
/// string is compiled into the binary, so a broken YAML is a broken build of
/// this crate, not a state a shipped binary can drift into; the pinning test
/// below calls this function, so CI fails first; and nothing else — not
/// `--help`, not `--schema`, not startup — calls it, so a broken spec can
/// only ever break `usage` itself.
fn operation() -> &'static Operation {
    static PARSED: OnceLock<Operation> = OnceLock::new();
    PARSED.get_or_init(|| {
        let mut spec = parse_spec("statistics", SPEC).expect("the bundled statistics spec parses");
        if spec.operations.len() != 1 {
            panic!(
                "the bundled statistics spec should describe exactly one operation, found {}",
                spec.operations.len()
            );
        }
        spec.operations.remove(0)
    })
}

pub fn command() -> Command {
    Command::new(COMMAND)
        .about("Show account/token usage by product and day (Statistics API)")
        .long_about(format!(
            "Show usage per Mapbox product, by day, for the account or one token.\n\n\
             Calls the Statistics API; the token needs the `statistics:read` scope. \
             `mapbox auth login` requests it by default; log in again if your stored \
             token predates that.\n\n\
             See {REPO_URL}/issues."
        ))
        .arg(
            Arg::new(TOKEN_ID_ARG)
                .long(TOKEN_ID_ARG)
                .value_name("ID")
                .help(
                    "Usage for one token instead of the whole account. \
             See `mapbox accounts list-tokens` for ids.",
                ),
        )
        .arg(
            Arg::new(PERIOD_START_ARG)
                .long(PERIOD_START_ARG)
                .value_name("YYYY-MM-DD")
                .help("Start of the usage period, inclusive. Defaults to 30 days ago."),
        )
        .arg(
            Arg::new(PERIOD_END_ARG)
                .long(PERIOD_END_ARG)
                .value_name("YYYY-MM-DD")
                .help(
                    "End of the usage period, inclusive. Defaults to today. \
                     At most 31 days after --period-start.",
                ),
        )
        .arg(
            Arg::new(PRODUCT_ARG)
                .long(PRODUCT_ARG)
                .value_name("NAME")
                .help(
                    "Only show this product. Matches the API's own name for it \
                     case-insensitively, exactly or as a substring (e.g. \"search box\" \
                     matches \"Search Box API - Requests\") — run without this flag first \
                     to see which names had usage. The API has no such filter itself; \
                     this narrows the response after it comes back.",
                ),
        )
        .arg(
            Arg::new(DAILY_ARG)
                .long(DAILY_ARG)
                .action(ArgAction::SetTrue)
                .help(
                    "List each product's usage day by day instead of a sparkline. \
                     Only changes -o text; -o json always has the daily figures.",
                ),
        )
}

pub fn run(
    matches: &ArgMatches,
    token: Option<&str>,
    debug: bool,
    timeout: Option<Duration>,
    mode: Mode,
) -> Result<()> {
    let mut json = fetch(&operation().base_url, matches, token, debug, timeout)?;
    let wanted_product = matches.get_one::<String>(PRODUCT_ARG);
    if let Some(wanted) = wanted_product {
        filter_to_product(&mut json, wanted)?;
    }
    // `output::emit_value`'s generic renderer can't table this (nested
    // `daily`/`dimensions` per product), so `render_text` is a dedicated
    // summary; `-o json` still gets the response untouched.
    let text = render_text(&json, matches.get_flag(DAILY_ARG), wanted_product.is_some());
    output::emit(mode, &text, json)
}

/// Keeps only the named product under `data.products`. Tries an exact,
/// case-insensitive match first (e.g. `vector tiles api`); if none exists,
/// falls back to a case-insensitive substring match (e.g. `search box`
/// against `Search Box API - Requests`), since the API's own product names
/// carry suffixes (`- Requests`, `for Web`, …) callers won't always know
/// ahead of time. Errors on no match, naming which products did have usage
/// (the same reasoning `output::pick_row`'s `--id` uses), or on more than
/// one substring match, naming the candidates so the caller can be exact.
fn filter_to_product(json: &mut Value, wanted: &str) -> Result<()> {
    let Some(products) = json.pointer("/data/products").and_then(Value::as_object) else {
        return Err(no_product_usage(wanted, &[]));
    };

    let matched = if let Some(exact) = products
        .keys()
        .find(|name| name.eq_ignore_ascii_case(wanted))
    {
        exact.clone()
    } else {
        let wanted_lower = wanted.to_ascii_lowercase();
        let mut candidates: Vec<&str> = products
            .keys()
            .map(String::as_str)
            .filter(|name| name.to_ascii_lowercase().contains(&wanted_lower))
            .collect();
        match candidates.len() {
            1 => candidates.remove(0).to_string(),
            0 => {
                let mut available: Vec<&str> = products.keys().map(String::as_str).collect();
                available.sort_unstable();
                return Err(no_product_usage(wanted, &available));
            }
            _ => {
                candidates.sort_unstable();
                return Err(ambiguous_product(wanted, &candidates));
            }
        }
    };

    // `products`'s borrow ended above, so this second lookup is fine.
    if let Some(products) = json
        .pointer_mut("/data/products")
        .and_then(Value::as_object_mut)
    {
        products.retain(|name, _| *name == matched);
    }
    Ok(())
}

fn no_product_usage(wanted: &str, available: &[&str]) -> anyhow::Error {
    let known = if available.is_empty() {
        "no product had any usage in this period".to_string()
    } else {
        format!("usage in this period exists for: {}", available.join(", "))
    };
    CliError::new(
        "not_found",
        format!("No product named `{wanted}` — {known}."),
    )
    .into()
}

fn ambiguous_product(wanted: &str, candidates: &[&str]) -> anyhow::Error {
    CliError::new(
        "not_found",
        format!(
            "`{wanted}` matches more than one product — {}. Pass one of those names exactly.",
            candidates.join(", ")
        ),
    )
    .into()
}

/// One product's name, total, and `(date, usage)` entries.
///
/// Two lifetimes, not one: the name outlives `calendar` (local to the match
/// arm in `render_text`), and `busiest` reads the name after the arm ends.
type ProductRow<'name, 'date> = (&'name str, i64, Vec<(&'date str, i64)>);

/// A product name longer than this truncates with an ellipsis; `--product`
/// still needs the full name.
const MAX_PRODUCT_NAME_WIDTH: usize = 28;

fn display_name(name: &str) -> String {
    if name.chars().count() <= MAX_PRODUCT_NAME_WIDTH {
        return name.to_string();
    }
    let kept: String = name
        .chars()
        .take(MAX_PRODUCT_NAME_WIDTH.saturating_sub(1))
        .collect();
    format!("{kept}…")
}

/// Person-readable summary of a Statistics API response: one line per
/// product (sparkline of daily values), or, under `--daily`, its usage
/// listed day by day. Falls back to pretty JSON if `data` is missing.
fn render_text(json: &Value, daily: bool, already_filtered: bool) -> String {
    let Some(data) = json.get("data") else {
        return pretty(json);
    };

    let mut lines = Vec::new();

    let period = data.get("period");
    let start = period.and_then(|p| p.get("start")).and_then(Value::as_str);
    let end = period.and_then(|p| p.get("end")).and_then(Value::as_str);
    lines.push(match (start, end) {
        (Some(start), Some(end)) => format!("Usage · {start} → {end}"),
        _ => "Usage".to_string(),
    });

    if let Some(token_id) = data.get("token_id").and_then(Value::as_str) {
        lines.push(format!("Token: {token_id}"));
    }

    lines.push(String::new());
    // Computed ahead of the match: `rows` is consumed rendering the table,
    // and the closing tips need to know how many products there were and
    // which was busiest.
    let mut product_count = 0;
    let mut busiest: Option<&str> = None;
    match data.get("products").and_then(Value::as_object) {
        Some(products) if !products.is_empty() => {
            // Padded to the full period so a day the API omitted still
            // shows as zero, rather than compressing the series.
            let calendar = calendar_for(data, products);

            let mut rows: Vec<ProductRow> = products
                .iter()
                .map(|(name, product)| {
                    let entries = daily_usage(product, &calendar);
                    (name.as_str(), total(&entries), entries)
                })
                .collect();
            // Busiest first, ties broken by name.
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

            product_count = rows.len();
            busiest = rows.first().map(|(name, ..)| *name);

            let name_width = rows
                .iter()
                .map(|(name, ..)| display_name(name).chars().count())
                .max()
                .unwrap_or(0);
            let total_width = rows
                .iter()
                .map(|(_, total, _)| with_thousands(*total).len())
                .max()
                .unwrap_or(0);

            if !daily {
                lines.push(format!(
                    "{:<name_width$}  {:>total_width$}  DAILY TREND",
                    "PRODUCT", "TOTAL"
                ));
            }
            // One blank line between rows, not after every one, for
            // readability without doubling the table's height.
            for (index, (name, total, entries)) in rows.into_iter().enumerate() {
                if index > 0 {
                    lines.push(String::new());
                }
                if daily {
                    lines.push(format!("{name} — total {}", with_thousands(total)));
                    let value_width = entries
                        .iter()
                        .map(|(_, usage)| with_thousands(*usage).len())
                        .max()
                        .unwrap_or(0);
                    // Newest day first; `entries` itself stays oldest-to-newest
                    // since the sparkline branch below needs that order.
                    for (date, usage) in entries.into_iter().rev() {
                        lines.push(format!("  {date}  {:>value_width$}", with_thousands(usage)));
                    }
                } else {
                    let values: Vec<i64> = entries.iter().map(|(_, usage)| *usage).collect();
                    lines.push(format!(
                        "{:<name_width$}  {:>total_width$}  {}",
                        display_name(name),
                        with_thousands(total),
                        sparkline(&values)
                    ));
                }
            }
        }
        _ => lines.push("No usage in this period.".to_string()),
    }

    if let Some(active_days) = data.get("activeDays").and_then(Value::as_array) {
        lines.push(String::new());
        lines.push(format!("Active days: {}", active_days.len()));
    }

    if let Some(generated_at) = json.get("generated_at").and_then(Value::as_str) {
        lines.push(String::new());
        lines.push(format!("Generated {generated_at}"));
    }

    let mut tips = vec![if daily {
        "`-o json` for the per-browser/country/host breakdown.".to_string()
    } else {
        "`-o json` for the exact per-day numbers and the per-browser/country/host breakdown."
            .to_string()
    }];
    if !daily {
        tips.push("`--daily` for the day-by-day numbers here instead of a sparkline.".to_string());
    }
    // Skip the hint once the caller has evidently already found the flag.
    if !already_filtered && product_count > 1 {
        if let Some(busiest) = busiest {
            tips.push(format!("`--product \"{busiest}\"` narrows to one product."));
        }
    }

    // The same shape every command's tips use: a lone tip reads `Tip: …`;
    // two or more get a `Tips:` header with each one indented below it.
    lines.push(String::new());
    if let [tip] = tips.as_slice() {
        lines.push(format!("Tip: {tip}"));
    } else {
        lines.push("Tips:".to_string());
        for tip in tips {
            lines.push(format!("  {tip}"));
        }
    }

    lines.join("\n")
}

/// `1234567` as `"1,234,567"`.
fn with_thousands(n: i64) -> String {
    let negative = n < 0;
    let digits = n.unsigned_abs().to_string();

    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let mut result: String = grouped.chars().rev().collect();
    if negative {
        result.insert(0, '-');
    }
    result
}

/// One product's usage per day. When `calendar` is non-empty, returns
/// exactly one entry per day in it, defaulting a date missing from the
/// product's own `daily` array to `0`. Falls back to the raw `daily` array
/// when `calendar` is empty (period couldn't be determined).
fn daily_usage<'a>(product: &'a Value, calendar: &'a [String]) -> Vec<(&'a str, i64)> {
    let reported = raw_daily(product);
    if calendar.is_empty() {
        return reported;
    }

    let by_date: std::collections::HashMap<&str, i64> = reported.into_iter().collect();
    calendar
        .iter()
        .map(|date| {
            (
                date.as_str(),
                by_date.get(date.as_str()).copied().unwrap_or(0),
            )
        })
        .collect()
}

/// One product's `daily` array as `(date, usage)` pairs.
fn raw_daily(product: &Value) -> Vec<(&str, i64)> {
    product
        .get("daily")
        .and_then(Value::as_array)
        .map(|daily| {
            daily
                .iter()
                .filter_map(|day| {
                    let date = day.get("date").and_then(Value::as_str)?;
                    let usage = day.get("usage").and_then(Value::as_i64).unwrap_or(0);
                    Some((date, usage))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn total(daily: &[(&str, i64)]) -> i64 {
    daily.iter().map(|(_, usage)| usage).sum()
}

/// Calendar days from `data.period`, as `YYYY-MM-DD` strings, or — if
/// `period` is missing or unparseable — the earliest to latest date any
/// product reported.
fn calendar_for(data: &Value, products: &serde_json::Map<String, Value>) -> Vec<String> {
    let period = data.get("period");
    let start = period.and_then(|p| p.get("start")).and_then(Value::as_str);
    let end = period.and_then(|p| p.get("end")).and_then(Value::as_str);
    if let (Some(start), Some(end)) = (start, end) {
        let days = calendar_days(start, end);
        if !days.is_empty() {
            return days;
        }
    }

    let mut dates: Vec<&str> = products
        .values()
        .flat_map(raw_daily)
        .map(|(date, _)| date)
        .collect();
    dates.sort_unstable();
    dates.dedup();
    match (dates.first(), dates.last()) {
        (Some(&first), Some(&last)) => calendar_days(first, last),
        _ => Vec::new(),
    }
}

/// Days from `start` to `end` inclusive; empty if either fails to parse, or
/// `end` comes before `start`.
fn calendar_days(start: &str, end: &str) -> Vec<String> {
    let (Some((sy, sm, sd)), Some((ey, em, ed))) = (parse_ymd(start), parse_ymd(end)) else {
        return Vec::new();
    };
    let first = days_from_civil(sy, sm, sd);
    let last = days_from_civil(ey, em, ed);
    if last < first {
        return Vec::new();
    }
    (first..=last)
        .map(|day| {
            let (y, m, d) = civil_from_days(day);
            format!("{y:04}-{m:02}-{d:02}")
        })
        .collect()
}

fn parse_ymd(date: &str) -> Option<(i64, u32, u32)> {
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    Some((year, month, day))
}

/// Date to day count, no calendar crate needed: Howard Hinnant's
/// `days_from_civil` (<https://howardhinnant.github.io/date_algorithms.html>).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// The inverse of [`days_from_civil`]. Also [`crate::events`]'s calendar.
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Block-element glyphs, low to high.
const SPARK_LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Sparkline scaled to the series' own min/max. A flat nonzero series uses
/// the middle glyph rather than the bottom one, which is reserved for an
/// actual zero-usage day.
fn sparkline(values: &[i64]) -> String {
    let (Some(&min), Some(&max)) = (values.iter().min(), values.iter().max()) else {
        return String::new();
    };

    if min == max {
        let level = if max > 0 { SPARK_LEVELS.len() / 2 } else { 0 };
        return SPARK_LEVELS[level].to_string().repeat(values.len());
    }

    values
        .iter()
        .map(|&value| {
            let scaled = (value - min) as f64 / (max - min) as f64;
            let index = (scaled * (SPARK_LEVELS.len() - 1) as f64).round() as usize;
            SPARK_LEVELS[index.min(SPARK_LEVELS.len() - 1)]
        })
        .collect()
}

fn pretty(json: &Value) -> String {
    serde_json::to_string_pretty(json).unwrap_or_else(|_| json.to_string())
}

/// Separate from [`run`] so a test can point it at a loopback server
/// instead of `api.mapbox.com`.
fn fetch(
    base_url: &str,
    matches: &ArgMatches,
    token: Option<&str>,
    debug: bool,
    timeout: Option<Duration>,
) -> Result<Value> {
    let client = http::client()?;
    let url = format!("{base_url}{}", operation().path_template);

    let mut query: Vec<(String, String)> = vec![];
    if let Some(t) = token {
        query.push((ACCESS_TOKEN.to_string(), t.to_string()));
    }
    // Sent under the spec's own name, not the flag's: `--token-id` goes out
    // as `token_id`, the same distinction `executor::dispatch` draws between
    // a `Parameter`'s `name` and its `arg_name`. Only spec parameters are
    // looked up here — `--product` and `--daily` filter and format the
    // response, and the API knows neither. A parameter the spec grew without
    // a matching flag in `command()` would not panic here: `get_one`/
    // `get_flag` on an unknown id return `None` in a release build (the
    // panic is `#[cfg(debug_assertions)]`-only), so it would silently drop
    // out of the request instead. The pinning test below is the actual
    // guard against that drift.
    //
    // Mirrors `executor::dispatch`'s `is_boolean` branch: today's three
    // parameters are all strings, but a future boolean one must not silently
    // vanish from the request the way a bare `get_one::<String>` would leave
    // it.
    for param in &operation().query_params {
        if param.is_boolean {
            if matches.get_flag(&param.arg_name) {
                query.push((param.name.clone(), "true".to_string()));
            }
            continue;
        }
        if let Some(value) = matches.get_one::<String>(&param.arg_name) {
            query.push((param.name.clone(), value.clone()));
        }
    }

    if debug {
        eprintln!("[debug] GET {}", redacted_url(&url, &query));
    }

    let response = http::send(
        client
            .get(&url)
            .query(&query)
            .timeout(http::budget(timeout, http::Payload::Bounded)),
    )
    .map_err(|e| executor::transport_failure("Request failed", e))?;

    let status = response.status();
    // Before `text()` consumes the response: a 5xx here is worth escalating,
    // and this is the only thing that lets support find the request.
    let request_id = executor::request_id(response.headers());
    let text = response
        .text()
        .map_err(|e| executor::transport_failure("Failed to read response", e))?;

    if !status.is_success() {
        return Err(CliError::http(status.as_u16(), &text)
            .with_request_id(request_id)
            .with_remedy(remedy_for(status.as_u16()))
            .into());
    }

    serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("Statistics API returned a body that isn't JSON: {e}"))
}

/// Request URL with the token redacted.
fn redacted_url(url: &str, query: &[(String, String)]) -> String {
    if query.is_empty() {
        return url.to_string();
    }
    let rendered: Vec<String> = query
        .iter()
        .map(|(name, value)| {
            let shown = if name == ACCESS_TOKEN {
                REDACTED
            } else {
                value.as_str()
            };
            format!("{name}={shown}")
        })
        .collect();
    format!("{url}?{}", rendered.join("&"))
}

/// Status-specific advice beyond the API's own `message`.
fn remedy_for(status: u16) -> Remedy {
    match status {
        401 => Remedy::default().with_fix(
            "The token is missing or invalid — run `mapbox auth login` again, or pass one \
             from account.mapbox.com with --token.",
        ),
        403 => Remedy::default().with_fix(
            "Check the token has the `statistics:read` scope — run `mapbox auth login` again \
             if it predates that scope. If it already has the scope, this account doesn't \
             have access to the Statistics API; contact Mapbox support.",
        ),
        422 => Remedy::default().with_fix(
            "--period-start/--period-end take YYYY-MM-DD, the end can't be before the start, \
             and the two may span at most 31 days.",
        ),
        429 => Remedy::default()
            .with_fix("Rate-limited at 20 requests per minute per IP. Wait and try again."),
        _ => Remedy::default(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn the_command_name_matches_its_own_constant() {
        assert_eq!(command().get_name(), COMMAND);
    }

    /// The spec supplies the request shape and `command()` declares the
    /// flags by hand, so nothing but this test holds the two together. It
    /// fails if the YAML gains or loses a query parameter, if one is
    /// renamed on either side, or if `--product`/`--daily` — this CLI's
    /// own, unknown to the API — ever appear in the spec.
    ///
    /// Deliberately literal on the expected side rather than reusing the
    /// `*_ARG` constants: renaming a flag through its constant would
    /// otherwise rename both sides of the comparison at once and prove
    /// nothing.
    #[test]
    fn the_spec_and_the_hand_declared_flags_describe_one_request() {
        let op = operation();
        assert_eq!(op.method, "GET", "`fetch` sends a GET");
        assert_eq!(op.base_url, "https://api.mapbox.com");
        assert_eq!(op.path_template, "/statistics/v1");

        let mut from_spec: Vec<&str> = op
            .query_params
            .iter()
            .map(|param| param.arg_name.as_str())
            .collect();
        from_spec.sort_unstable();
        assert_eq!(
            from_spec,
            ["period-end", "period-start", "token-id"],
            "the spec's query parameters and the flags `command()` declares have drifted"
        );

        // Each of those has to be an argument `fetch` can look up by that
        // name, or `get_one` panics at request time.
        let usage = command();
        let declared: Vec<&str> = usage
            .get_arguments()
            .map(|arg| arg.get_id().as_str())
            .collect();
        for arg_name in from_spec {
            assert!(
                declared.contains(&arg_name),
                "the spec has `{arg_name}`, `command()` declares {declared:?}"
            );
        }
    }

    /// The example from the Statistics API doc linked in the module docs.
    fn documented_example() -> Value {
        serde_json::json!({
            "data": {
                "period": { "start": "2024-01-01", "end": "2024-01-02" },
                "token_id": "abc123",
                "activeDays": ["2024-01-01", "2024-01-02"],
                "products": {
                    "Vector Tiles API": {
                        "daily": [
                            { "date": "2024-01-01", "usage": 100 },
                            { "date": "2024-01-02", "usage": 100 }
                        ]
                    }
                }
            },
            "generated_at": "2024-01-31T10:30:00Z"
        })
    }

    #[test]
    fn the_summary_totals_each_products_daily_usage() {
        let text = render_text(&documented_example(), false, false);
        assert!(text.contains("Usage · 2024-01-01 → 2024-01-02"), "{text}");
        assert!(text.contains("Token: abc123"), "{text}");
        let row = text
            .lines()
            .find(|line| line.contains("Vector Tiles API"))
            .expect("the product has its own line");
        assert!(row.contains("200"), "{row}"); // 100 + 100, not just the first
        assert!(text.contains("Active days: 2"), "{text}");
        assert!(text.contains("2024-01-31T10:30:00Z"), "{text}");
    }

    #[test]
    fn per_day_dates_and_numbers_stay_out_of_the_text_summary() {
        let text = render_text(&documented_example(), false, false);
        // Each date appears once, naming the period's boundaries — not
        // again per product, which is where a per-day breakdown would
        // have repeated it.
        assert_eq!(text.matches("2024-01-01").count(), 1, "{text}");
        assert_eq!(text.matches("2024-01-02").count(), 1, "{text}");
        let product_line = text
            .lines()
            .find(|line| line.contains("Vector Tiles API"))
            .expect("the product has its own line");
        assert!(
            SPARK_LEVELS
                .iter()
                .any(|glyph| product_line.contains(*glyph)),
            "expected a sparkline glyph on the product's line: {product_line}"
        );
    }

    #[test]
    fn daily_lists_each_day_instead_of_a_sparkline() {
        let text = render_text(&documented_example(), true, false);
        assert!(text.contains("Vector Tiles API — total 200"), "{text}");
        assert!(text.contains("2024-01-01"), "{text}");
        assert!(text.contains("2024-01-02"), "{text}");
        assert_eq!(
            text.matches("100").count(),
            2,
            "one `100` per day, not a total standing in for both:\n{text}"
        );
        assert!(
            !SPARK_LEVELS.iter().any(|glyph| text.contains(*glyph)),
            "no sparkline glyph should appear under --daily:\n{text}"
        );
    }

    #[test]
    fn daily_lists_the_newest_day_first() {
        let text = render_text(&documented_example(), true, false);
        let newest = text
            .lines()
            .position(|line| line.trim_start().starts_with("2024-01-02"))
            .expect("2024-01-02 appears");
        let oldest = text
            .lines()
            .position(|line| line.trim_start().starts_with("2024-01-01"))
            .expect("2024-01-01 appears");
        assert!(newest < oldest, "{text}");
    }

    #[test]
    fn a_hint_toward_daily_appears_only_without_it() {
        let with_sparkline = render_text(&documented_example(), false, false);
        assert!(with_sparkline.contains("--daily"), "{with_sparkline}");

        let with_daily = render_text(&documented_example(), true, false);
        assert!(!with_daily.contains("--daily"), "{with_daily}");
    }

    #[test]
    fn a_hint_toward_product_names_the_busiest_one_unless_already_filtered() {
        let json = serde_json::json!({
            "data": {
                "products": {
                    "Quiet API": { "daily": [{ "date": "2026-01-01", "usage": 1 }] },
                    "Busy API": { "daily": [{ "date": "2026-01-01", "usage": 99 }] }
                }
            }
        });

        let unfiltered = render_text(&json, false, false);
        assert!(
            unfiltered.contains("--product \"Busy API\""),
            "{unfiltered}"
        );

        let filtered = render_text(&json, false, true);
        assert!(
            !filtered.contains("--product"),
            "a response already narrowed by --product should not suggest it again:\n{filtered}"
        );
    }

    #[test]
    fn a_single_product_response_gets_no_product_hint() {
        let json = serde_json::json!({
            "data": { "products": { "Only API": { "daily": [{ "date": "2026-01-01", "usage": 1 }] } } }
        });
        let text = render_text(&json, false, false);
        assert!(
            !text.contains("--product"),
            "nothing left to narrow when there is already only one product:\n{text}"
        );
    }

    #[test]
    fn with_thousands_groups_from_the_right() {
        assert_eq!(with_thousands(0), "0");
        assert_eq!(with_thousands(1), "1");
        assert_eq!(with_thousands(999), "999");
        assert_eq!(with_thousands(1000), "1,000");
        assert_eq!(with_thousands(1234567), "1,234,567");
        assert_eq!(with_thousands(-1234), "-1,234");
    }

    #[test]
    fn display_name_leaves_short_names_alone() {
        assert_eq!(display_name("Vector Tiles API"), "Vector Tiles API");
    }

    #[test]
    fn display_name_truncates_a_long_name_with_an_ellipsis() {
        let long = "Navigation SDK Core Framework - Active Guidance Trips";
        let shown = display_name(long);
        assert_eq!(shown.chars().count(), MAX_PRODUCT_NAME_WIDTH);
        assert!(shown.ends_with('…'), "{shown}");
        assert!(long.starts_with(shown.trim_end_matches('…')), "{shown}");
    }

    #[test]
    fn the_sparkline_table_has_a_column_header() {
        let text = render_text(&documented_example(), false, false);
        assert!(
            text.contains("PRODUCT") && text.contains("TOTAL") && text.contains("DAILY TREND"),
            "{text}"
        );
    }

    #[test]
    fn the_daily_listing_has_no_column_header() {
        let text = render_text(&documented_example(), true, false);
        assert!(!text.contains("DAILY TREND"), "{text}");
    }

    #[test]
    fn a_large_total_is_comma_grouped_in_both_display_modes() {
        let json = serde_json::json!({
            "data": {
                "products": {
                    "Directions API": {
                        "daily": [{ "date": "2026-01-01", "usage": 1234567 }]
                    }
                }
            }
        });
        assert!(
            render_text(&json, false, false).contains("1,234,567"),
            "sparkline mode"
        );
        assert!(
            render_text(&json, true, false).contains("1,234,567"),
            "daily mode"
        );
    }

    #[test]
    fn tips_are_labeled_and_each_on_their_own_line() {
        let text = render_text(&documented_example(), false, false);
        let tips_at = text
            .lines()
            .position(|line| line == "Tips:")
            .expect("a Tips: header line");
        let after: Vec<&str> = text.lines().skip(tips_at + 1).collect();
        assert!(!after.is_empty(), "{text}");
        for line in &after {
            assert!(line.starts_with("  `"), "not an indented tip line: {line}");
        }
    }

    #[test]
    fn the_busiest_product_is_listed_first() {
        let json = serde_json::json!({
            "data": {
                "products": {
                    "Quiet API": { "daily": [{ "date": "2026-01-01", "usage": 1 }] },
                    "Busy API": { "daily": [{ "date": "2026-01-01", "usage": 99 }] }
                }
            }
        });
        let text = render_text(&json, false, false);
        let busy = text.find("Busy API").expect("Busy API is listed");
        let quiet = text.find("Quiet API").expect("Quiet API is listed");
        assert!(
            busy < quiet,
            "the busier product should sort first:\n{text}"
        );
    }

    #[test]
    fn product_rows_have_breathing_room_between_them_but_not_a_trailing_gap() {
        // Not single letters: the header row's own words contain enough of
        // the alphabet to match a one-letter product name by accident.
        let json = serde_json::json!({
            "data": {
                "products": {
                    "Alpha Product": { "daily": [{ "date": "2026-01-01", "usage": 1 }] },
                    "Bravo Product": { "daily": [{ "date": "2026-01-01", "usage": 2 }] },
                    "Charlie Product": { "daily": [{ "date": "2026-01-01", "usage": 3 }] }
                }
            },
            "generated_at": "2026-01-02T00:00:00Z"
        });
        let text = render_text(&json, false, false);
        let lines: Vec<&str> = text.lines().collect();

        let product_line = |name: &str| lines.iter().position(|l| l.contains(name)).unwrap();
        // Busiest first: Charlie (3), Bravo (2), Alpha (1).
        assert_eq!(
            product_line("Bravo Product") - product_line("Charlie Product"),
            2,
            "{text}"
        );
        assert_eq!(
            product_line("Alpha Product") - product_line("Bravo Product"),
            2,
            "{text}"
        );

        let last_row = product_line("Alpha Product");
        assert!(lines[last_row + 1].is_empty(), "{text}");
        assert!(lines[last_row + 2].starts_with("Generated"), "{text}");
    }

    #[test]
    fn no_products_reads_as_no_usage_rather_than_an_empty_table() {
        let json = serde_json::json!({ "data": { "products": {} } });
        assert!(render_text(&json, false, false).contains("No usage in this period."));
    }

    #[test]
    fn a_flat_nonzero_series_reads_as_steady_rather_than_idle() {
        assert_eq!(sparkline(&[5, 5, 5]), "▅▅▅");
    }

    #[test]
    fn an_all_zero_series_reads_as_idle() {
        assert_eq!(sparkline(&[0, 0, 0]), "▁▁▁");
    }

    #[test]
    fn the_extremes_of_a_series_take_the_extreme_glyphs() {
        let chars: Vec<char> = sparkline(&[0, 50, 100]).chars().collect();
        assert_eq!(chars[0], '▁');
        assert_eq!(chars[2], '█');
    }

    #[test]
    fn an_empty_series_is_an_empty_sparkline() {
        assert_eq!(sparkline(&[]), "");
    }

    #[test]
    fn filtering_to_a_product_keeps_only_that_one() {
        let mut json = serde_json::json!({
            "data": {
                "products": {
                    "Vector Tiles API": { "daily": [{ "date": "2026-01-01", "usage": 5 }] },
                    "Directions API": { "daily": [{ "date": "2026-01-01", "usage": 9 }] }
                }
            }
        });
        filter_to_product(&mut json, "Vector Tiles API").expect("a real product name filters");

        let products = json["data"]["products"]
            .as_object()
            .expect("still an object");
        assert_eq!(products.len(), 1, "{products:?}");
        assert!(products.contains_key("Vector Tiles API"), "{products:?}");
    }

    #[test]
    fn filtering_matches_the_product_name_regardless_of_case() {
        let mut json = serde_json::json!({
            "data": { "products": { "Vector Tiles API": { "daily": [] } } }
        });
        filter_to_product(&mut json, "VECTOR TILES api").expect("case-insensitive match");
        assert!(json["data"]["products"]
            .as_object()
            .expect("still an object")
            .contains_key("Vector Tiles API"));
    }

    #[test]
    fn filtering_falls_back_to_a_substring_match() {
        let mut json = serde_json::json!({
            "data": { "products": { "Search Box API - Requests": { "daily": [] } } }
        });
        filter_to_product(&mut json, "search box").expect("a substring match");
        assert!(json["data"]["products"]
            .as_object()
            .expect("still an object")
            .contains_key("Search Box API - Requests"));
    }

    #[test]
    fn filtering_prefers_an_exact_match_over_a_substring_one() {
        let mut json = serde_json::json!({
            "data": {
                "products": {
                    "Vector Tiles API": { "daily": [] },
                    "Vector Tiles API - Legacy": { "daily": [] }
                }
            }
        });
        filter_to_product(&mut json, "Vector Tiles API").expect("the exact name wins");
        let products = json["data"]["products"]
            .as_object()
            .expect("still an object");
        assert_eq!(products.len(), 1, "{products:?}");
        assert!(products.contains_key("Vector Tiles API"), "{products:?}");
    }

    #[test]
    fn filtering_a_substring_matching_more_than_one_product_names_the_candidates() {
        let mut json = serde_json::json!({
            "data": {
                "products": {
                    "Static Images API": { "daily": [] },
                    "Static Tiles API": { "daily": [] }
                }
            }
        });
        let err =
            filter_to_product(&mut json, "static").expect_err("an ambiguous substring is an error");
        let message = err.to_string();
        assert!(message.contains("Static Images API"), "{message}");
        assert!(message.contains("Static Tiles API"), "{message}");
    }

    #[test]
    fn filtering_to_an_unknown_product_names_what_did_have_usage() {
        let mut json = serde_json::json!({
            "data": { "products": { "Vector Tiles API": { "daily": [] } } }
        });
        let err = filter_to_product(&mut json, "Not A Real Product")
            .expect_err("an unknown product name is an error");
        let message = err.to_string();
        assert!(message.contains("Not A Real Product"), "{message}");
        assert!(message.contains("Vector Tiles API"), "{message}");
    }

    #[test]
    fn filtering_when_nothing_had_usage_says_so_rather_than_listing_nothing() {
        let mut json = serde_json::json!({ "data": { "products": {} } });
        let err = filter_to_product(&mut json, "Vector Tiles API")
            .expect_err("no products at all is still a miss");
        assert!(
            err.to_string().contains("no product had any usage"),
            "{err}"
        );
    }

    #[test]
    fn calendar_days_spans_a_month_and_a_leap_day() {
        assert_eq!(
            calendar_days("2026-02-27", "2026-03-01"),
            vec!["2026-02-27", "2026-02-28", "2026-03-01"],
            "2026 is not a leap year: no Feb 29"
        );
        assert_eq!(
            calendar_days("2024-02-27", "2024-03-01"),
            vec!["2024-02-27", "2024-02-28", "2024-02-29", "2024-03-01"],
            "2024 is a leap year"
        );
    }

    #[test]
    fn calendar_days_is_empty_for_an_unparseable_or_backwards_range() {
        assert!(calendar_days("not-a-date", "2026-01-02").is_empty());
        assert!(
            calendar_days("2026-01-05", "2026-01-01").is_empty(),
            "end before start"
        );
    }

    #[test]
    fn civil_days_round_trip() {
        for (y, m, d) in [
            (2024, 2, 29),  // leap day
            (2025, 12, 31), // year boundary
            (2026, 1, 1),
            (2026, 9, 8),
        ] {
            let z = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(z), (y, m, d), "round trip for {y}-{m}-{d}");
        }
    }

    #[test]
    fn a_day_missing_from_dailys_own_array_becomes_an_explicit_zero() {
        let json = serde_json::json!({
            "data": {
                "period": { "start": "2026-01-01", "end": "2026-01-03" },
                "products": {
                    "Address Autofill": {
                        // The middle day is absent, not zero.
                        "daily": [
                            { "date": "2026-01-01", "usage": 1 },
                            { "date": "2026-01-03", "usage": 1 }
                        ]
                    }
                }
            }
        });
        let text = render_text(&json, false, false);
        let line = text
            .lines()
            .find(|l| l.contains("Address Autofill"))
            .expect("the product line exists");
        let glyphs = line.chars().filter(|c| SPARK_LEVELS.contains(c)).count();
        assert_eq!(glyphs, 3, "expected one glyph per calendar day: {line}");
    }

    #[test]
    fn a_response_with_no_data_field_falls_back_to_pretty_json() {
        let json = serde_json::json!({ "message": "not what was expected" });
        let text = render_text(&json, false, false);
        assert!(text.contains("not what was expected"), "{text}");
    }

    #[test]
    fn the_token_is_redacted_but_nothing_else_is() {
        let query = vec![
            (ACCESS_TOKEN.to_string(), "pk.super.secret".to_string()),
            ("period_start".to_string(), "2026-01-01".to_string()),
        ];
        let rendered = redacted_url("https://api.mapbox.com/statistics/v1", &query);
        assert!(!rendered.contains("pk.super.secret"), "{rendered}");
        assert!(rendered.contains("access_token=<redacted>"), "{rendered}");
        assert!(rendered.contains("period_start=2026-01-01"), "{rendered}");
    }

    #[test]
    fn a_url_with_no_query_is_left_alone() {
        assert_eq!(
            redacted_url("https://api.mapbox.com/statistics/v1", &[]),
            "https://api.mapbox.com/statistics/v1"
        );
    }

    /// A loopback server standing in for `api.mapbox.com`; returns the
    /// request head it received, and the address to send `fetch` at.
    fn serve_once(
        status_line: &'static str,
        body: &'static str,
    ) -> (std::thread::JoinHandle<String>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("the client's connection");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));

            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }

            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            String::from_utf8_lossy(&head).into_owned()
        });

        (server, format!("http://{addr}"))
    }

    #[test]
    fn a_success_response_comes_back_as_the_parsed_body() {
        let (server, base_url) = serve_once(
            "200 OK",
            r#"{"data":{"products":{}},"generated_at":"2026-01-01T00:00:00Z"}"#,
        );

        let matches = command().get_matches_from(["usage"]);
        let json = fetch(&base_url, &matches, Some("pk.test"), false, None).expect("a 200 parses");
        assert_eq!(json["generated_at"], "2026-01-01T00:00:00Z");

        let head = server.join().expect("the server thread");
        assert!(
            head.starts_with(&format!(
                "GET {}?access_token=pk.test",
                operation().path_template
            )),
            "{head}"
        );
    }

    #[test]
    fn optional_query_params_reach_the_request_under_their_api_names() {
        let (server, base_url) = serve_once("200 OK", r#"{"data":{}}"#);

        let matches = command().get_matches_from([
            "usage",
            "--token-id",
            "tok123",
            "--period-start",
            "2026-01-01",
            "--period-end",
            "2026-01-31",
        ]);
        fetch(&base_url, &matches, Some("pk.test"), false, None).expect("a 200 parses");

        let head = server.join().expect("the server thread");
        for expected in [
            "token_id=tok123",
            "period_start=2026-01-01",
            "period_end=2026-01-31",
        ] {
            assert!(
                head.contains(expected),
                "missing `{expected}` in request line: {head}"
            );
        }
    }

    /// The API answers 403, not 401, when the token itself is fine but is
    /// missing `statistics:read` — confirmed live against a real account. A
    /// re-login fixes that case, so the fix has to lead with it rather than
    /// jump straight to Mapbox support, which is only the answer once the
    /// scope is already there.
    #[test]
    fn a_403_checks_the_scope_before_pointing_at_mapbox_support() {
        let (server, base_url) = serve_once(
            "403 Forbidden",
            r#"{"message":"This API requires a token with statistics:read scope."}"#,
        );

        let matches = command().get_matches_from(["usage"]);
        let err = fetch(&base_url, &matches, Some("pk.test"), false, None)
            .expect_err("a 403 is an error");
        server.join().expect("the server thread");

        let cli = err
            .downcast::<CliError>()
            .expect("an HTTP failure is a CliError");
        assert_eq!(cli.status, Some(403));
        let fix = cli.fix.as_deref().unwrap_or_default();
        assert!(fix.contains("statistics:read"), "{fix:?}");
        assert!(fix.contains("Mapbox support"), "{fix:?}");
        assert!(
            fix.find("statistics:read").unwrap() < fix.find("Mapbox support").unwrap(),
            "the self-serviceable fix should come before the support fallback: {fix:?}"
        );
    }

    /// A 401 here means the token itself is missing or invalid — not a scope
    /// problem, which this endpoint answers with 403 instead. Naming the
    /// scope on a 401 would send the reader chasing something a bad token
    /// can't have anyway.
    #[test]
    fn a_401_says_the_token_is_invalid_rather_than_naming_a_scope() {
        let (server, base_url) = serve_once(
            "401 Unauthorized",
            r#"{"message":"Not Authorized - Invalid Token"}"#,
        );

        let matches = command().get_matches_from(["usage"]);
        let err = fetch(&base_url, &matches, Some("pk.test"), false, None)
            .expect_err("a 401 is an error");
        server.join().expect("the server thread");

        let cli = err
            .downcast::<CliError>()
            .expect("an HTTP failure is a CliError");
        let fix = cli.fix.as_deref().unwrap_or_default();
        assert!(fix.contains("auth login"), "{fix:?}");
        assert!(!fix.contains("statistics:read"), "{fix:?}");
    }
}
