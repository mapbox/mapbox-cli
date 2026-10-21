# `commands search` query sets

Queries that `mapbox commands search` is scored against. The `eval_set_*`
tests in `src/commands.rs` run every file here and fail when a set ranks
more than `SLACK` (3) queries fewer right than its recorded score, so a
reworded summary that moves one query does not fail the build.
`cargo test eval_set -- --nocapture` prints each set's score and every
query that did not rank first. On a PR, CI's `commands search eval` job puts the
scores in its job summary, with the queries the change fixed and broke
against the base branch. It also says when there was nothing to compare
against, and why: a push with no base, a base from before these sets, or a
base run that failed. `scripts/test-commands-search-report.sh` checks the
summary against hand-written reports.

## Format

Tab-separated, one query per line, `#` lines are comments:

| Column | Meaning |
| --- | --- |
| query | What a person or an agent types, as one string. |
| expected | The command that should rank first, as `mapbox …`. `\|` separates several acceptable answers; `-` means the CLI cannot do this and nothing should come back. |
| kind | What sort of query it is (below). |
| source | Where the phrasing came from (`issues.tsv` and `docs.tsv` only). |

A query counts as right when its first result is one of the expected
commands, or, for `-`, when the result is empty. Top 3 counts it when an
expected command is anywhere in the first three.

## The sets

The sets differ mostly in how much the search has been tuned against them.
The more it was, the better the score looks and the less it says about a
query nobody has seen.

| File | Rows | Source | Tuned against |
| --- | --- | --- | --- |
| `tuning.tsv` | 100 | Written by hand while building the search | Heavily: scoring and help text were changed until these ranked well |
| `issues.tsv` | 100 | Mapbox GitHub issue and help-center titles, lightly rewritten | Partly: its misses were read to choose which kinds of fix to make |
| `docs.tsv` | 60 | Task phrasings from Mapbox docs, blog posts and tutorials | Least: unseen until the last round, then used once to choose between two changes |

Kinds:

- `agent`: worded the way the CLI's own text is, as an agent that has read
  `--help` would ("get sprite json").
- `human`: a person's own words ("which fonts do I have").
- `synonym`: a word the command's text says differently ("rename my style").
- `typo`: a realistic misspelling ("uplaod a font").
- `symptom`: a bug report rather than a task ("estimate area returns 0 km2").
  The search is not built for these; they are a secondary measure.
- `none`: a task the CLI cannot do ("build a store locator").

## Reading the scores

- A drop in `tuning.tsv` or `issues.tsv` means something that worked broke.
- `docs.tsv` is the best estimate of how the search does on unseen
  phrasing. A gain there is the one worth believing.
- Keyword search has a ceiling: a query that shares no words with any
  command's text cannot be found. Most of the remaining misses are of that
  kind, and tuning will not fix them.
- A `-` row can only be won when no word of the query matches anything,
  because matches are cut relative to the best one, never at a fixed
  minimum (see `MIN_SHARE_OF_BEST`). A task the CLI cannot do almost always
  shares a word with some command, so those rows are lost today, and the
  rows a set can actually win are fewer than its total:

  | File | Rows | `-` rows | `-` rows won | Rows that can win |
  | --- | --- | --- | --- | --- |
  | `tuning.tsv` | 100 | 2 | 2 | 100 |
  | `issues.tsv` | 100 | 11 | 0 | 89 |
  | `docs.tsv` | 60 | 11 | 0 | 49 |

  They stay in the score, rather than being scored apart, so a set's score
  keeps one meaning across changes, and a change that starts answering
  "nothing" for them shows up as a gain in the same number. `tuning.tsv`
  wins its two only because "bake a cake" and "deploy to kubernetes" share
  no word with any command.

## Limits

- Stack Overflow, GIS StackExchange and Reddit could not be fetched while
  `issues.tsv` and `docs.tsv` were gathered, so neither has queries from
  them, and both lean toward developer phrasing.
- Many `docs.tsv` sources are a page title seen in search results, not a page
  that was read in full.
- The expected command is a judgment call for some rows, which is why some
  list more than one.

## Changing them

- Record a set's new score in `EVAL_SETS` when a change improves it, in the
  same commit. Dropping past the slack is a decision to explain in the PR.
- Fix the kind of miss rather than special-casing a query.
- Put new real queries, such as ones the team actually typed, in a new file
  rather than these, so there is always a set the search has not been
  fitted to. Add it to `EVAL_SETS` with its own test.
