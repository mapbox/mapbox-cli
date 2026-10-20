//! `mapbox commands` — finding a command by what it does.
//!
//! `search` ranks every command against a description of a task; `list` is
//! every command with its summary, on one page. Both read the command
//! surface [`crate::schema`] builds, so there is no second description of
//! the CLI to keep in step, and both answer for people and agents alike: a
//! table at a terminal, `[{command, summary}]` under `json`.
//!
//! The search is local keyword matching and nothing more: no model, no
//! request, no index on disk. At a few dozen commands, scoring every one on
//! each call takes no measurable time, and anything smarter would be a
//! dependency to ship for a list a person could read in a minute. The cost
//! is that it only finds words the command's own text uses, or one of a
//! handful of synonyms for them — "upload a worker script" will not find
//! `deploy` — so the summaries are what decide how well it works.
//!
//! The query is a free string, so telemetry records its length and never
//! its text (see [`crate::telemetry_event`]). No token, no request.

use std::collections::BTreeSet;

use anyhow::Result;
use clap::{value_parser, Arg, ArgMatches, Command};
use serde_json::{json, Value};

use crate::help_layout;
use crate::output::{self, Mode};
use crate::schema::{self, CommandEntry};
use crate::spec::ServiceSpec;

pub const COMMAND: &str = "commands";

const DEFAULT_LIMIT: usize = 5;

/// How much each part of a command counts toward a match, after
/// `cf cli search`'s: a word in the command's own name says far more than
/// the same word somewhere in one of its arguments' help.
const NAME_WEIGHT: f64 = 8.0;
const SUMMARY_WEIGHT: f64 = 5.0;
const DESCRIPTION_WEIGHT: f64 = 2.0;
/// The group's own one-line description. Many summaries are a few words
/// ("Tilequery", "Forward geocoding"), and the group is often where the
/// plainer word is: `tilesets` "query features at a point".
const GROUP_WEIGHT: f64 = 1.5;
const ARGUMENTS_WEIGHT: f64 = 0.5;
/// How much a term has to score in one command before it counts as
/// answered in full: an exact match anywhere but an argument's help, or a
/// near miss in the name or summary. A word found only in an argument's
/// help, or near-missed in a description or group line, answers it in part,
/// so a passing mention cannot lift a command to full coverage of the query.
const FULL_MATCH: f64 = GROUP_WEIGHT;
/// Taken off for each word of a command's name the query did not ask for,
/// so "delete a style" finds `styles delete` ahead of `styles draft
/// delete`, whose summary and description match it just as well.
const UNASKED_NAME_WORD: f64 = 1.0;

/// What a `batch` command's score is scaled by when the query did not ask
/// for many.
const UNASKED_BATCH: f64 = 0.5;

/// What a deprecated command's score is scaled by. Still found, so a script
/// that names one can be told what it was, but below anything it would be
/// better to start using.
const DEPRECATED: f64 = 0.5;

/// How well a query term matches a word, by kind of match (see
/// [`similarity`]).
const EXACT: f64 = 1.0;
const PREFIX: f64 = 0.7;
const STEM: f64 = 0.5;
const TYPO: f64 = 0.4;

/// A match this far below the best one is noise beside it.
///
/// Relative only. A fixed minimum also emptied the answer for tasks this CLI
/// cannot do, but it emptied it just as readily for ones it can, worded
/// loosely: "how far can I cycle in twenty minutes" finds `isochrone` with
/// one word. A wrong guess costs the caller a look; no answer when there was
/// one costs them the command.
const MIN_SHARE_OF_BEST: f64 = 0.1;

/// Words that describe the asking rather than the task. Left in, "how do I
/// list my styles" would rank whatever says "my" most.
///
/// "mapbox" is one too: every command is Mapbox's, so it only rewards the
/// commands whose text happens to repeat the name.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "for", "from", "how", "i", "in",
    "into", "is", "it", "mapbox", "me", "my", "of", "on", "or", "the", "this", "to", "up", "want",
    "what", "with", "you", "your",
];

/// Words people use for something the commands' own text says another way,
/// each with the words it stands in for. One way only: "remove" finds
/// `delete`, but "delete" does not go looking for "remove". Keys are in the
/// form [`tokens`] leaves them, plural `s` gone.
const SYNONYMS: &[(&str, &[&str])] = &[
    ("signin", &["login"]),
    ("signout", &["logout"]),
    ("remove", &["delete"]),
    ("discard", &["delete"]),
    ("erase", &["delete"]),
    ("edit", &["update"]),
    ("rename", &["update"]),
    ("modify", &["update"]),
    ("change", &["update"]),
    // Not "many" or "several": they as often describe what comes back
    // ("a reverse lookup that returns several results") as what goes in.
    ("bulk", &["multiple", "batch"]),
    ("picture", &["image"]),
    ("png", &["image"]),
    ("jpg", &["image"]),
    ("thumbnail", &["image"]),
    ("screenshot", &["image"]),
    ("permission", &["scope"]),
    ("saved", &["stored"]),
    ("add", &["upload"]),
];

/// A synonym's match counts for less than the word itself, so a command
/// that uses the caller's own word still comes first.
const SYNONYM_WEIGHT: f64 = 0.8;

pub fn command() -> Command {
    Command::new(COMMAND)
        .about("Find a command by what it does")
        .long_about(
            "Find a command by what it does. `search` ranks every command against a \
             description of a task; `list` shows every command with its summary. \
             Both are local: no request and no token.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("search")
                .about("Find the commands that match a task, best first")
                .arg(
                    Arg::new("query")
                        .required(true)
                        .num_args(1..)
                        .help("What you want to do, in a few words: \"upload a font\""),
                )
                .arg(
                    Arg::new("limit")
                        .long("limit")
                        .value_parser(value_parser!(usize))
                        .default_value(DEFAULT_LIMIT.to_string())
                        .help("How many matches to show; 0 shows every match"),
                ),
        )
        .subcommand(Command::new("list").about("List every command with its summary"))
}

pub fn search(
    app: &Command,
    specs: &[ServiceSpec],
    matches: &ArgMatches,
    mode: Mode,
) -> Result<()> {
    let query = matches
        .get_many::<String>("query")
        .expect("required")
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let limit = *matches.get_one::<usize>("limit").expect("has a default");

    let mut found = rank(app, specs, &query);
    if limit > 0 {
        found.truncate(limit);
    }
    if found.is_empty() {
        output::progress(&format!(
            "No command matches \"{query}\". `mapbox commands list` shows every command."
        ));
    }
    emit(mode, &found)
}

pub fn list(app: &Command, specs: &[ServiceSpec], mode: Mode) -> Result<()> {
    let mut all = schema::build(app, specs, &[]).commands;
    all.sort_by(|a, b| a.command.cmp(&b.command));
    emit(mode, &all)
}

/// The commands that match `query`, best first, down to
/// [`MIN_SHARE_OF_BEST`].
fn rank(app: &Command, specs: &[ServiceSpec], query: &str) -> Vec<CommandEntry> {
    let index = index(app, specs);
    let order = rank_in(&index, query);
    let mut entries: Vec<Option<CommandEntry>> =
        index.into_iter().map(|(_, entry)| Some(entry)).collect();
    order
        .into_iter()
        .map(|i| entries[i].take().expect("each position once"))
        .collect()
}

/// Every command `search` can answer with, and its words. Built once per
/// run, or once for a whole test set.
fn index(app: &Command, specs: &[ServiceSpec]) -> Vec<(Fields, CommandEntry)> {
    schema::build(app, specs, &[])
        .commands
        .into_iter()
        // The caller is already here, and "find a command" would otherwise
        // rank this group's own summaries first.
        .filter(|entry| entry.service != COMMAND)
        .map(|entry| (Fields::of(app, &entry), entry))
        .collect()
}

/// Positions in `index` of the commands that match `query`, best first.
/// Ties go to the shorter command name, then alphabetically, so the same
/// query always answers the same way.
fn rank_in(index: &[(Fields, CommandEntry)], query: &str) -> Vec<usize> {
    let vocabulary: BTreeSet<&str> = index
        .iter()
        .flat_map(|(fields, _)| fields.parts.iter().flat_map(|(_, words)| words))
        .map(String::as_str)
        .collect();
    let query = Query::parse(query, &vocabulary);
    if query.terms.is_empty() {
        return vec![];
    }
    let mut scored: Vec<(f64, usize)> = index
        .iter()
        .enumerate()
        .map(|(i, (fields, entry))| (score(&query, fields, entry.deprecated), i))
        .collect();
    scored.sort_by(|(a_score, a), (b_score, b)| {
        let (a, b) = (&index[*a].1.command, &index[*b].1.command);
        b_score
            .total_cmp(a_score)
            .then(a.len().cmp(&b.len()))
            .then(a.cmp(b))
    });
    above_floor(scored).into_iter().map(|(_, i)| i).collect()
}

/// What is left of `scored`, already sorted best first, once the zero scores
/// and the ones too far below the best are dropped.
fn above_floor<T>(scored: Vec<(f64, T)>) -> Vec<(f64, T)> {
    let floor = scored
        .first()
        .map_or(0.0, |(best, _)| best * MIN_SHARE_OF_BEST);
    scored
        .into_iter()
        .take_while(|(score, _)| *score > 0.0 && *score >= floor)
        .collect()
}

struct Query {
    terms: Vec<Term>,
    /// Neighboring words written together, "log out" as "logout", tried
    /// only for an exact match, with the two words they were made from.
    /// Kept apart from `terms` so that one that matches nothing — almost all
    /// of them — does not count as a word of the query going unanswered.
    compounds: Vec<(String, [String; 2])>,
}

struct Term {
    word: String,
    /// Whether a word a typo away counts. Not for a word some command's text
    /// uses as it is: that is a real word, not a misspelling, and matching
    /// it one edit off turns "match" into "batch".
    typos: bool,
}

impl Query {
    fn parse(query: &str, vocabulary: &BTreeSet<&str>) -> Self {
        let all: Vec<String> = tokens(query).collect();
        let mut seen = BTreeSet::new();
        // Before stopwords come out: the "in" of "log in" is one.
        let compounds = all
            .windows(2)
            .map(|pair| (pair.concat(), [pair[0].clone(), pair[1].clone()]))
            // "log in log in" asks once.
            .filter(|(compound, _)| seen.insert(compound.clone()))
            .collect();
        Query {
            terms: terms(query)
                .into_iter()
                .map(|word| Term {
                    typos: !vocabulary.contains(word.as_str()),
                    word,
                })
                .collect(),
            compounds,
        }
    }
}

/// A command's text, as the words each weighted part of it holds.
struct Fields {
    name: BTreeSet<String>,
    parts: Vec<(f64, BTreeSet<String>)>,
}

impl Fields {
    fn of(app: &Command, entry: &CommandEntry) -> Self {
        let name = entry
            .command
            .strip_prefix("mapbox ")
            .unwrap_or(&entry.command);
        // The help list's line before the command's own `about`: for an API
        // group, `about` is the spec's title ("Mapbox Tokens API"), which
        // names the API rather than what it does.
        let group = help_layout::description(&entry.service)
            .map(ToString::to_string)
            .or_else(|| {
                app.find_subcommand(&entry.service)
                    .and_then(Command::get_about)
                    .map(ToString::to_string)
            })
            .unwrap_or_default();
        let arguments = entry
            .arguments
            .iter()
            .flat_map(|arg| [Some(arg.name.as_str()), arg.description.as_deref()])
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let name = words(name);
        Fields {
            name: name.clone(),
            parts: vec![
                (NAME_WEIGHT, name),
                (SUMMARY_WEIGHT, words(&entry.summary)),
                (
                    DESCRIPTION_WEIGHT,
                    words(entry.description.as_deref().unwrap_or_default()),
                ),
                (GROUP_WEIGHT, words(&group)),
                (ARGUMENTS_WEIGHT, words(&arguments)),
            ],
        }
    }
}

/// Each term counts once per part, at its best match there, so a long
/// description does not outrank a short summary by repeating a word. The
/// total is then scaled by how much of the query matched (see
/// [`FULL_MATCH`]): a command that answers the whole task beats one that
/// answers half of it loudly.
fn score(query: &Query, fields: &Fields, deprecated: bool) -> f64 {
    let mut total = 0.0;
    // A matched compound answers the words it was made from, which on
    // their own usually match nothing: "out" of "log out".
    let mut answered = BTreeSet::new();
    for (compound, parts) in &query.compounds {
        for (weight, words) in &fields.parts {
            let best = words
                .iter()
                .map(|word| exact_or_synonym(compound, word))
                .fold(0.0, f64::max);
            if best > 0.0 {
                total += weight * best;
                answered.extend(parts.iter().cloned());
            }
        }
    }
    let mut matched = 0.0;
    for term in &query.terms {
        let mut term_total = 0.0;
        for (weight, words) in &fields.parts {
            let best = words
                .iter()
                .map(|word| relatedness(term, word))
                .fold(0.0, f64::max);
            term_total += weight * best;
        }
        matched += if answered.contains(&term.word) {
            1.0
        } else {
            (term_total / FULL_MATCH).min(1.0)
        };
        total += term_total;
    }
    let mut total = (total - UNASKED_NAME_WORD * unasked_name_words(query, fields) as f64).max(0.0);
    // A `batch` command does what a sibling does, for many inputs at once,
    // and its text says so at length: "forward or reverse", every option the
    // sibling has. Unless the query asks for many, the sibling is the answer.
    if fields.name.contains("batch") && !asks_for_many(query) {
        total *= UNASKED_BATCH;
    }
    if deprecated {
        total *= DEPRECATED;
    }
    let coverage = matched / query.terms.len() as f64;
    total * coverage * coverage
}

/// How many words of a command's name no term or compound of the query
/// asked for.
fn unasked_name_words(query: &Query, fields: &Fields) -> usize {
    fields
        .name
        .iter()
        .filter(|word| {
            query
                .terms
                .iter()
                .all(|term| relatedness(term, word) == 0.0)
                && !query
                    .compounds
                    .iter()
                    .any(|(compound, _)| exact_or_synonym(compound, word) > 0.0)
        })
        .count()
}

/// Whether the query says it has many inputs. A typo does not: "patch" and
/// "watch" are one letter from "batch", and a query that means one of them
/// is not asking for a `batch` command.
fn asks_for_many(query: &Query) -> bool {
    query.terms.iter().any(|term| {
        ["batch", "multiple"].iter().any(|many| {
            similarity(&term.word, many, false) > 0.0 || synonyms(&term.word).contains(many)
        })
    })
}

/// [`similarity`], or a synonym's exact match if that is closer. A synonym
/// is not fuzzy-matched: it is already a guess, and "batch" one letter off
/// is "match".
fn relatedness(term: &Term, word: &str) -> f64 {
    let direct = similarity(&term.word, word, term.typos);
    if synonyms(&term.word).contains(&word) {
        direct.max(SYNONYM_WEIGHT)
    } else {
        direct
    }
}

/// Compounds are only ever matched whole: "logout", not a word it starts.
fn exact_or_synonym(compound: &str, word: &str) -> f64 {
    if compound == word {
        1.0
    } else if synonyms(compound).contains(&word) {
        SYNONYM_WEIGHT
    } else {
        0.0
    }
}

fn synonyms(term: &str) -> &'static [&'static str] {
    SYNONYMS
        .iter()
        .find(|(key, _)| *key == term)
        .map_or(&[], |(_, synonyms)| synonyms)
}

/// How well a query term matches one word of a command's text, from 0 to 1.
/// `typos` allows a match one or two edits off.
fn similarity(term: &str, word: &str, typos: bool) -> f64 {
    if term == word {
        return EXACT;
    }
    let term_len = term.chars().count();
    // Typing the start of a word: "geo" for "geocoder".
    if term_len >= 3 && word.starts_with(term) {
        return PREFIX;
    }
    // The same stem, spelled on for a different ending: "geocoding" and
    // "geocoder".
    let shared = term
        .chars()
        .zip(word.chars())
        .take_while(|(a, b)| a == b)
        .count();
    if shared >= 5 {
        return STEM;
    }
    if !typos {
        return 0.0;
    }
    // Short words are left out: up to four letters, one edit turns too many
    // words into another real one — "shop" into "show" — and up to seven,
    // two do — "convert" into "content".
    let edits = strsim::damerau_levenshtein(term, word);
    let allowed = match term_len.min(word.chars().count()) {
        0..=4 => 0,
        5..=7 => 1,
        _ => 2,
    };
    if edits <= allowed {
        return TYPO;
    }
    0.0
}

/// The query's words, minus the ones that only describe the asking.
fn terms(query: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    raw_tokens(query)
        // Before the plural `s` comes off, which would leave "this" as "thi".
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .map(singular)
        .filter(|word| seen.insert(word.clone()))
        .collect()
}

fn words(text: &str) -> BTreeSet<String> {
    tokens(text).collect()
}

/// Lowercased runs of letters and digits, each with a plural `s` taken off,
/// so "styles" in a query matches "style" in a summary. Cruder than
/// stemming, and enough for the words this CLI's commands use.
fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    raw_tokens(text).map(singular)
}

fn raw_tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

fn singular(word: String) -> String {
    match word.strip_suffix('s') {
        Some(stem) if stem.chars().count() >= 3 && !stem.ends_with('s') => stem.to_string(),
        _ => word,
    }
}

fn emit(mode: Mode, entries: &[CommandEntry]) -> Result<()> {
    // No table, not an empty line: `text` with no match prints nothing, so
    // `mapbox commands search … > file` leaves an empty file. `json` still
    // answers `[]`, the one shape a parser can take.
    if entries.is_empty() && !mode.is_json() {
        return Ok(());
    }
    let width = entries
        .iter()
        .map(|entry| entry.command.len())
        .max()
        .unwrap_or(0)
        .max("COMMAND".len());
    let text = std::iter::once(format!("{:width$}  SUMMARY", "COMMAND"))
        .chain(
            entries
                .iter()
                .map(|entry| format!("{:width$}  {}", entry.command, entry.summary)),
        )
        .collect::<Vec<_>>()
        .join("\n");
    let json = entries
        .iter()
        .map(|entry| json!({ "command": entry.command, "summary": entry.summary }))
        .collect();
    output::emit(mode, &text, Value::Array(json))?;
    if !mode.is_json() {
        output::print_tips(&["`mapbox <command> --help` shows what one takes.".to_string()]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(query: &str) -> Vec<String> {
        let specs = crate::spec::effective_services().expect("specs load");
        let app = crate::build_app(&specs);
        rank(&app, &specs, query)
            .into_iter()
            .take(3)
            .map(|entry| entry.command)
            .collect()
    }

    /// A query set under `tests/fixtures/commands_search/`, and the fewest
    /// rows each must keep answering right: first result, and anywhere in
    /// the first three. A `-` row is right when nothing comes back.
    ///
    /// The floors are the scores when they were last raised. A change that
    /// improves one raises its floor in the same commit, so the gain cannot
    /// quietly be given back; one that lowers a floor says why in its PR.
    /// New real queries go in a new file rather than into these: a set the
    /// search has been tuned against stops saying how it does on the next
    /// query nobody has seen.
    const EVAL_SETS: &[(&str, &str, usize, usize)] = &[
        (
            "tuning.tsv",
            include_str!("../tests/fixtures/commands_search/tuning.tsv"),
            84,
            97,
        ),
        (
            "issues.tsv",
            include_str!("../tests/fixtures/commands_search/issues.tsv"),
            50,
            67,
        ),
        (
            "docs.tsv",
            include_str!("../tests/fixtures/commands_search/docs.tsv"),
            22,
            30,
        ),
    ];

    /// Where [`eval_set_keeps_its_scores`] writes its reports, when set.
    const REPORT_DIR_ENV: &str = "MAPBOX_SEARCH_EVAL_REPORT";

    /// Holds one set to its floors. `cargo test eval_set -- --nocapture`
    /// prints the scores, and each query that did not rank first. One test
    /// per set, so they run in parallel.
    ///
    /// With [`REPORT_DIR_ENV`] set, also writes every query's result to
    /// `<dir>/<set>`, before anything is asserted, so a failing run still
    /// leaves one: a set missing from CI's summary would read as a set
    /// nobody added. A row that cannot be scored goes in the report as a
    /// `# invalid` line rather than stopping the run there.
    fn eval_set_keeps_its_scores(wanted: &str) {
        let &(name, text, min_first, min_top3) = EVAL_SETS
            .iter()
            .find(|(name, ..)| *name == wanted)
            .unwrap_or_else(|| panic!("`{wanted}` is not in EVAL_SETS"));
        let specs = crate::spec::effective_services().expect("specs load");
        let app = crate::build_app(&specs);
        let index = index(&app, &specs);
        // From the index rather than the whole schema: an expected
        // `commands` command is one search never answers with.
        let known: BTreeSet<&str> = index
            .iter()
            .map(|(_, entry)| entry.command.as_str())
            .collect();

        let (mut rows, mut first, mut top3) = (0, 0, 0);
        let (mut misses, mut invalid) = (vec![], vec![]);
        let mut report = format!("# floors\t{min_first}\t{min_top3}\n");
        for line in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        {
            let mut columns = line.split('\t');
            let (Some(query), Some(expected)) = (columns.next(), columns.next()) else {
                invalid.push(format!("no expected column: {line:?}"));
                continue;
            };
            let expected: Vec<&str> = match expected {
                "-" => vec![],
                answers => answers.split('|').collect(),
            };
            // A renamed command would leave its row unwinnable, and the
            // floor would read that as the search getting worse.
            if let Some(command) = expected.iter().find(|c| !known.contains(**c)) {
                invalid.push(format!(
                    "`{command}` is not a command search answers with (\"{query}\")"
                ));
                continue;
            }
            let found: Vec<String> = rank_in(&index, query)
                .into_iter()
                .take(3)
                .map(|i| index[i].1.command.clone())
                .collect();
            let (is_first, in_top3) = if expected.is_empty() {
                (found.is_empty(), found.is_empty())
            } else {
                (
                    found
                        .first()
                        .is_some_and(|c| expected.contains(&c.as_str())),
                    found.iter().any(|c| expected.contains(&c.as_str())),
                )
            };
            report.push_str(&format!(
                "{query}\t{}\t{}\t{}\t{}\n",
                expected.join("|"),
                found.join("|"),
                u8::from(is_first),
                u8::from(in_top3)
            ));
            rows += 1;
            first += usize::from(is_first);
            top3 += usize::from(in_top3);
            if !is_first {
                misses.push(format!("  \"{query}\": want {expected:?}, got {found:?}"));
            }
        }
        for problem in &invalid {
            report.push_str(&format!("# invalid\t{problem}\n"));
        }
        if let Some(dir) = std::env::var_os(REPORT_DIR_ENV) {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).expect("create the report directory");
            std::fs::write(dir.join(name), report).expect("write the report");
        }

        eprintln!(
            "{name}: {first}/{rows} first (floor {min_first}), \
             {top3}/{rows} in top 3 (floor {min_top3})"
        );
        for miss in &misses {
            eprintln!("{miss}");
        }
        assert!(invalid.is_empty(), "{name}:\n{}", invalid.join("\n"));
        assert!(
            first >= min_first && top3 >= min_top3,
            "{name} fell below its floor: {first} first (floor {min_first}), \
             {top3} in top 3 (floor {min_top3}). If the trade-off is intended — \
             help text made accurate at the cost of a match, say — lower the floor \
             in EVAL_SETS to {}/{} and say why in the PR. The misses are above.",
            first.min(min_first),
            top3.min(min_top3),
        );
        if first > min_first || top3 > min_top3 {
            eprintln!("  above its floor: raise it to {first}/{top3} in EVAL_SETS");
        }
    }

    /// A file added to the directory but not to [`EVAL_SETS`] would be a
    /// set that is never scored.
    #[test]
    fn every_query_file_is_an_eval_set() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/commands_search"
        );
        let mut files: Vec<String> = std::fs::read_dir(dir)
            .expect("read the fixture directory")
            .map(|entry| entry.expect("a directory entry").file_name())
            .filter_map(|name| name.into_string().ok())
            .filter(|name| name.ends_with(".tsv"))
            .collect();
        files.sort();
        let mut sets: Vec<&str> = EVAL_SETS.iter().map(|(name, ..)| *name).collect();
        sets.sort_unstable();
        assert_eq!(files, sets);
    }

    #[test]
    fn eval_set_tuning() {
        eval_set_keeps_its_scores("tuning.tsv");
    }

    #[test]
    fn eval_set_issues() {
        eval_set_keeps_its_scores("issues.tsv");
    }

    #[test]
    fn eval_set_docs() {
        eval_set_keeps_its_scores("docs.tsv");
    }

    /// Phrasings a person or an agent might actually type, each held to the
    /// command it should find first. A change to the scoring that loses one
    /// of these has made the search worse for that caller.
    #[test]
    fn a_task_finds_its_command_first() {
        for (query, expected) in [
            ("upload a font", "mapbox fonts upload"),
            ("list my styles", "mapbox styles list"),
            ("delete a style", "mapbox styles delete"),
            ("geocode an address", "mapbox geocoder forward"),
            ("driving route between two points", "mapbox directions"),
            ("travel time matrix", "mapbox matrix"),
            ("snap gps trace to roads", "mapbox map-match"),
            ("static map image", "mapbox static get-image"),
            ("log in", "mapbox auth login"),
            ("list access tokens", "mapbox accounts list-tokens"),
            ("shell completion", "mapbox completion"),
            ("install mcp server", "mapbox mcp install"),
            ("find coffee shops near me", "mapbox search category"),
            ("query features at a point", "mapbox tilesets query"),
            ("vector tile", "mapbox tilesets get-mvt"),
            ("add an icon to a sprite", "mapbox sprites upload"),
            ("download a style", "mapbox styles download"),
            ("reachable area in 10 minutes", "mapbox isochrone"),
            ("feedback about the cli", "mapbox report"),
            ("sign in", "mapbox auth login"),
            ("discard a style draft", "mapbox styles draft delete"),
            ("rename my style", "mapbox styles update"),
            (
                "bulk upload icons to a sprite",
                "mapbox sprites upload-batch",
            ),
            ("add a ttf font", "mapbox fonts upload"),
            ("publish a tileset", "mapbox tilesets-cli"),
            ("turn off telemetry", "mapbox config set"),
            ("check my proxy settings", "mapbox doctor"),
            ("uninstall the cli", "mapbox uninstall"),
        ] {
            let found = top(query);
            assert_eq!(
                found.first().map(String::as_str),
                Some(expected),
                "\"{query}\" ranked {found:?}"
            );
        }
    }

    #[test]
    fn a_typo_still_finds_the_command() {
        assert_eq!(
            top("uplaod font").first().map(String::as_str),
            Some("mapbox fonts upload")
        );
    }

    #[test]
    fn a_query_of_only_stopwords_matches_nothing() {
        assert!(top("how do I").is_empty());
    }

    #[test]
    fn nonsense_matches_nothing() {
        assert!(top("xyzzy").is_empty());
    }

    #[test]
    fn matches_far_below_the_best_are_dropped() {
        let kept: Vec<&str> = above_floor(vec![(10.0, "best"), (1.0, "tenth"), (0.9, "noise")])
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        assert_eq!(kept, ["best", "tenth"]);
    }

    const WEAK_QUERY: &str = "how far can i cycle in twenty minutes";

    /// A loosely worded task matched by one word is still answered: a
    /// fixed minimum score once emptied this. Not a phrasing from any query
    /// set, so it does not lean on one the search was fitted to.
    #[test]
    fn a_weak_only_match_is_still_an_answer() {
        assert_eq!(
            top(WEAK_QUERY).first().map(String::as_str),
            Some("mapbox isochrone")
        );
    }

    /// `geocoder batch`'s description names everything a single lookup
    /// does, so without the discount it ranks first for a single query.
    #[test]
    fn batch_comes_second_unless_asked_for() {
        assert_eq!(
            top("geocoding results inside a bbox")
                .first()
                .map(String::as_str),
            Some("mapbox geocoder forward")
        );
        assert_eq!(
            top("bulk geocode addresses").first().map(String::as_str),
            Some("mapbox geocoder batch")
        );
    }

    /// "patch" and "match" are a letter from "batch", and both are words a
    /// command's text uses as they are, so neither asks for many.
    #[test]
    fn a_word_one_letter_from_batch_does_not_find_a_batch_command() {
        for query in ["patch a style", "match a style"] {
            let found = top(query);
            assert!(
                found.first().is_some_and(|c| !c.contains("batch")),
                "\"{query}\" ranked {found:?}"
            );
        }
    }

    #[test]
    fn a_word_some_command_uses_is_not_a_typo() {
        let vocabulary = BTreeSet::from(["match"]);
        let query = Query::parse("match uplaod", &vocabulary);
        let typos: Vec<bool> = query.terms.iter().map(|term| term.typos).collect();
        assert_eq!(typos, [false, true]);
        assert_eq!(relatedness(&query.terms[0], "batch"), 0.0);
    }

    /// The caller is already in `commands`, and its own summaries would
    /// otherwise answer every query about finding a command.
    #[test]
    fn the_commands_group_is_left_out() {
        let specs = crate::spec::effective_services().expect("specs load");
        let app = crate::build_app(&specs);
        let found = rank(&app, &specs, "find a command search list commands");
        assert!(
            found.iter().all(|entry| entry.service != COMMAND),
            "{:?}",
            found.iter().map(|entry| &entry.command).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_repeated_pair_is_one_compound() {
        let query = Query::parse("log in log in", &BTreeSet::new());
        let compounds: Vec<&str> = query.compounds.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(compounds, ["login", "inlog"]);
    }

    #[test]
    fn stopwords_are_dropped_before_the_plural_is() {
        assert!(terms("what is this").is_empty());
    }

    fn term(word: &str) -> Term {
        Term {
            word: word.to_string(),
            typos: true,
        }
    }

    #[test]
    fn a_synonym_counts_for_less_than_the_word_itself() {
        assert_eq!(relatedness(&term("remove"), "remove"), EXACT);
        assert_eq!(relatedness(&term("remove"), "delete"), SYNONYM_WEIGHT);
        assert_eq!(relatedness(&term("delete"), "remove"), 0.0);
    }

    /// "bulk" stands in for "batch", and "batch" one letter off is
    /// "match": the stand-in is matched exactly, not within a typo.
    #[test]
    fn a_synonym_is_not_fuzzy_matched() {
        assert_eq!(relatedness(&term("bulk"), "batch"), SYNONYM_WEIGHT);
        assert_eq!(similarity("batch", "match", true), TYPO);
        assert_eq!(relatedness(&term("bulk"), "match"), 0.0);
    }

    #[test]
    fn a_plural_matches_its_singular() {
        assert_eq!(
            similarity(&terms("styles")[0], &terms("style")[0], true),
            EXACT
        );
    }

    #[test]
    fn short_words_are_not_fuzzy_matched() {
        assert_eq!(similarity("map", "mcp", true), 0.0);
        assert_eq!(similarity("shop", "show", true), 0.0);
    }

    /// Letters, not bytes: "café" is four letters, too short for a typo.
    #[test]
    fn lengths_count_letters() {
        assert_eq!(similarity("cafè", "café", true), 0.0);
        assert_eq!(singular("éés".to_string()), "éés");
    }
}
