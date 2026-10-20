#!/usr/bin/env python3
"""Markdown summary of `mapbox commands search` eval results, for a CI job
summary.

    commands-search-report.py --head DIR --head-status N
                              [--base DIR --base-status N] [--sets DIR]
                              [--comment-dir DIR]

Each directory holds the per-set reports the `eval_set_*` tests write when
MAPBOX_SEARCH_EVAL_REPORT names it; each status is the exit code of the
`cargo test` run that wrote them. No `--base` means there was no base to run
(not a pull request). `--sets` is the fixture directory, so a set whose
report is missing is named rather than left out.

With `--comment-dir`, also writes what the PR comment workflow
(.github/workflows/search-eval-comment.yml) needs: `state`, one of `drop`
(some set dropped by LARGE_DROP_QUERIES or LARGE_DROP_POINTS against the
base), `ok` (compared, no large drop) or `none` (nothing to compare), and,
for `drop`, `body.md`.

The status is what tells apart a base that predates the eval (the run passed
and wrote nothing) from one whose run failed: the reports alone look the
same for both.

Reports, never gates: the floors in `cargo test` are the gate. So this never
fails the job on bad input: a malformed line is skipped and counted, and an
unreadable report is named. Standard library only, so the job needs no
install step.
"""

import argparse
import sys
from pathlib import Path


class Set:
    def __init__(self):
        self.floors = None
        self.rows = {}
        self.invalid = []
        self.skipped = 0
        self.unreadable = None


def load(directory):
    """Every report in `directory`, by set name. Empty when there is none."""
    sets = {}
    if directory is None or not directory.is_dir():
        return sets
    for path in sorted(directory.iterdir()):
        report = sets[path.name] = Set()
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            report.unreadable = str(error)
            continue
        for line in text.splitlines():
            parse_line(report, line)
    return sets


def parse_line(report, line):
    columns = line.split("\t")
    if columns[0] == "# floors":
        try:
            report.floors = (int(columns[1]), int(columns[2]))
        except (IndexError, ValueError):
            report.skipped += 1
        return
    if columns[0] == "# invalid":
        report.invalid.append(" ".join(columns[1:]))
        return
    if columns[0].startswith("#") or not line.strip():
        return
    if len(columns) != 5 or columns[3] not in ("0", "1") or columns[4] not in ("0", "1"):
        report.skipped += 1
        return
    query, expected, found, first, top3 = columns
    report.rows[query] = {
        "expected": " or ".join(expected.split("|")) if expected else "(nothing)",
        "found": ", ".join(found.split("|")) if found else "(nothing)",
        "first": first == "1",
        "top3": top3 == "1",
    }


def cell(text):
    """`text` made safe for one Markdown table cell."""
    return (
        str(text)
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace("|", "\\|")
        .replace("\n", " ")
    )


def score(report, key):
    return sum(row[key] for row in report.rows.values())


# A set losing this many first-place queries against the base, or this many
# percentage points of its first-place share, gets the PR a comment. Smaller
# trade-offs stay in the job summary: the floors already make each one a
# decision in the diff, and a comment for every one would be noise.
LARGE_DROP_QUERIES = 3
LARGE_DROP_POINTS = 5

# Rows of "no longer first" per set in a comment; the job summary has the rest.
COMMENT_ROWS = 20

# Under the table, so a reader need not know the tests to read it.
LEGEND = (
    "**Ranked first**: the expected command was the first result. "
    "**In top 3**: it was among the first three. "
    "**Required**: the floor in `EVAL_SETS` (src/commands.rs); a lower score fails `cargo test`. "
    "A query the CLI cannot serve counts as right only when nothing comes back. "
    "How each set was gathered is in tests/fixtures/commands_search/README.md."
)


def count(head, total, base):
    """`22 (37%)`, with the change against the base when there is one:
    `22 (37%, +1)`."""
    share = f"{round(100 * head / total)}%" if total else "-"
    change = head - base if base is not None else 0
    return f"{head} ({share}, {change:+d})" if change else f"{head} ({share})"


def changed(head_rows, base_rows):
    """Queries in both whose first result went from right to wrong or back."""
    fixed, broken = [], []
    for query, row in head_rows.items():
        before = base_rows.get(query)
        if before is None or before["first"] == row["first"]:
            continue
        line = (
            f"| {cell(query)} | {cell(row['expected'])} | "
            f"{cell(before['found'])} | {cell(row['found'])} |"
        )
        (fixed if row["first"] else broken).append(line)
    return fixed, broken


def status_note(status):
    return "did not finish" if status is None else f"failed (exit {status})"


def parse_status(text):
    try:
        return int(text)
    except (TypeError, ValueError):
        return None


def base_note(base_given, base, base_status):
    """Why there is nothing to compare against, or what to know about it."""
    if not base_given:
        return "Not a pull request, so there is no base to compare against."
    if not base:
        if base_status == 0:
            return (
                "The base predates the eval (its run had no eval tests), "
                "so only this branch's scores are shown."
            )
        return (
            f"The base run {status_note(base_status)} before writing "
            "any report, so only this branch's scores are shown."
        )
    if base_status != 0:
        return (
            f"The base run {status_note(base_status)}; "
            "its scores are as far as it got."
        )
    return None


def notes(report, floors_missed):
    out = []
    if report.unreadable:
        out.append(f"unreadable: {report.unreadable}")
    if floors_missed:
        out.append("**below its floor**")
    if report.invalid:
        out.append(f"{len(report.invalid)} invalid rows")
    if report.skipped:
        out.append(f"{report.skipped} malformed lines skipped")
    return out


def render(head, head_status, base, base_given, base_status, expected_sets):
    out = ["## `commands search` eval", ""]
    if not head:
        if head_status == 0:
            out.append("No reports were written, though the eval run passed: no `eval_set_*` test ran.")
        else:
            out.append(
                f"The eval run on this branch {status_note(head_status)} "
                "before writing any report. The build job's test step has the details."
            )
        return out
    if head_status != 0:
        out += [
            f"The eval run on this branch {status_note(head_status)}: a set is "
            "below its floor, a row is invalid, or the tests did not build. The build "
            "job's test step has the details.",
            "",
        ]
    note = base_note(base_given, base, base_status)
    if note:
        out += [note, ""]
    compare = bool(base)

    out += [
        "| Query set | Queries | Ranked first | In top 3 | Required (first / top 3) | Note |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    details = []
    names = sorted(set(head) | set(base) | set(expected_sets))
    for name in names:
        label = f"`{cell(name)}`"
        if name not in head:
            where = "only in the base" if name in base else "missing from this run"
            out.append(f"| {label} | | | | | {where} |")
            continue
        report = head[name]
        base_report = base.get(name)
        first, top3 = score(report, "first"), score(report, "top3")
        base_first = score(base_report, "first") if base_report else None
        base_top3 = score(base_report, "top3") if base_report else None
        floors = report.floors
        floor = f"{floors[0]} / {floors[1]}" if floors else "-"
        missed = bool(floors) and (first < floors[0] or top3 < floors[1])
        row_notes = notes(report, missed)
        if compare and base_report is None:
            row_notes.insert(0, "only in this branch")
        out.append(
            f"| {label} | {len(report.rows)} | {count(first, len(report.rows), base_first)} | "
            f"{count(top3, len(report.rows), base_top3)} | {floor} | {cell('; '.join(row_notes))} |"
        )

        if report.invalid:
            details += [f"### {label}: rows that could not be scored", ""]
            details += [f"- {cell(problem)}" for problem in report.invalid] + [""]

        if base_report:
            fixed, broken = changed(report.rows, base_report.rows)
            header = ["| Query | Want | Base | This branch |", "| --- | --- | --- | --- |"]
            if broken:
                details += [f"### {label}: {len(broken)} no longer first", ""] + header + broken + [""]
            if fixed:
                details += [f"### {label}: {len(fixed)} now first", ""] + header + fixed + [""]

        misses = [
            f"| {cell(query)} | {cell(row['expected'])} | {cell(row['found'])} |"
            for query, row in report.rows.items()
            if not row["first"]
        ]
        if misses:
            details += [
                f"<details><summary><code>{cell(name)}</code>: {len(misses)} not first</summary>",
                "",
                "| Query | Want | Got (top 3) |",
                "| --- | --- | --- |",
                *misses,
                "",
                "</details>",
                "",
            ]

    return out + ["", LEGEND, "", *details]


def share(first, total):
    return 100 * first / total if total else 0.0


def large_drops(head, base):
    """Each set that dropped far enough against the base to comment on, as
    (name, base first, base rows, head first, head rows)."""
    drops = []
    for name in sorted(set(head) & set(base)):
        report, base_report = head[name], base[name]
        first, base_first = score(report, "first"), score(base_report, "first")
        rows, base_rows = len(report.rows), len(base_report.rows)
        lost = base_first - first
        points = share(base_first, base_rows) - share(first, rows)
        if lost >= LARGE_DROP_QUERIES or points >= LARGE_DROP_POINTS:
            drops.append((name, base_first, base_rows, first, rows))
    return drops


def comment(head, head_status, base, base_status):
    """The comment state, and its body when there is a large drop."""
    if not head or not base or base_status != 0 or head_status is None:
        return "none", None
    drops = large_drops(head, base)
    if not drops:
        return "ok", None
    out = [
        "### `commands search` ranks fewer queries first than the base",
        "",
        "| Query set | Base | This PR | Change |",
        "| --- | --- | --- | --- |",
    ]
    details = []
    for name, base_first, base_rows, first, rows in drops:
        points = share(base_first, base_rows) - share(first, rows)
        out.append(
            f"| `{cell(name)}` | {count(base_first, base_rows, None)} | "
            f"{count(first, rows, None)} | {first - base_first:+d} queries, "
            f"{-points:+.0f} points |"
        )
        _, broken = changed(head[name].rows, base[name].rows)
        if broken:
            shown = broken[:COMMENT_ROWS]
            more = len(broken) - len(shown)
            details += [
                f"<details><summary><code>{cell(name)}</code>: {len(broken)} no longer first</summary>",
                "",
                "| Query | Want | Base | This PR |",
                "| --- | --- | --- | --- |",
                *shown,
                *([f"\n{more} more in the job summary."] if more else []),
                "",
                "</details>",
                "",
            ]
    out += [
        "",
        f"A set losing {LARGE_DROP_QUERIES} or more first-place queries, or "
        f"{LARGE_DROP_POINTS} or more points, posts this. If the trade-off is "
        "intended, say why in the PR; the floors in `EVAL_SETS` decide whether "
        "`cargo test` passes.",
        "",
        *details,
    ]
    return "drop", "\n".join(out)


def write_comment(directory, state, body):
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "state").write_text(state + "\n", encoding="utf-8")
    if body is not None:
        (directory / "body.md").write_text(body + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--head", type=Path, required=True)
    parser.add_argument("--head-status", default="")
    parser.add_argument("--base", type=Path)
    parser.add_argument("--base-status", default="")
    parser.add_argument("--sets", type=Path)
    parser.add_argument("--comment-dir", type=Path)
    args = parser.parse_args()

    expected_sets = []
    if args.sets is not None and args.sets.is_dir():
        expected_sets = [path.name for path in args.sets.glob("*.tsv")]
    head, base = load(args.head), load(args.base)
    head_status, base_status = parse_status(args.head_status), parse_status(args.base_status)
    lines = render(head, head_status, base, args.base is not None, base_status, expected_sets)
    print("\n".join(lines))
    if args.comment_dir is not None:
        write_comment(args.comment_dir, *comment(head, head_status, base, base_status))


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as error:  # noqa: BLE001 - a summary, never a gate
        # The job is promised not to fail on this script; say what broke in
        # the summary itself rather than leaving it empty.
        print("## `commands search` eval\n")
        print(f"The summary script failed: `{cell(repr(error))}`. This is a bug in the script.")
        sys.exit(0)
