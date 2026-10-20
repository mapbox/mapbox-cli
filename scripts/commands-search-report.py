#!/usr/bin/env python3
"""Markdown summary of `mapbox commands search` eval results, for a CI job
summary.

    commands-search-report.py <head-dir> [<base-dir>]

Each directory holds the per-set reports the `eval_set_*` tests write when
MAPBOX_SEARCH_EVAL_REPORT names it. With a base, the summary says which
queries the change fixed and broke; without one (a base from before the
eval existed), only the scores.

Reports, never gates: the floors in `cargo test` are the gate. Standard
library only, so the job needs no install step.
"""

import sys
from pathlib import Path


def load(directory):
    sets = {}
    if directory is None or not directory.is_dir():
        return sets
    for path in sorted(directory.iterdir()):
        floors, rows = None, {}
        for line in path.read_text().splitlines():
            columns = line.split("\t")
            if columns[0] == "# floors":
                floors = (int(columns[1]), int(columns[2]))
                continue
            query, expected, found, first, top3 = columns
            rows[query] = {
                "expected": expected.replace("|", " or ") or "(nothing)",
                "found": found.replace("|", ", ") or "(nothing)",
                "first": first == "1",
                "top3": top3 == "1",
            }
        sets[path.name] = (floors, rows)
    return sets


def score(rows, key):
    return sum(row[key] for row in rows.values())


def delta(head, base):
    if base is None:
        return f"{head}"
    change = head - base
    return f"{head} ({change:+d})" if change else f"{head}"


def changed(head_rows, base_rows):
    """Queries in both whose first result went from right to wrong or back."""
    fixed, broken = [], []
    for query, row in head_rows.items():
        before = base_rows.get(query)
        if before is None or before["first"] == row["first"]:
            continue
        line = (
            f"| {query} | {row['expected']} | {before['found']} | {row['found']} |"
        )
        (fixed if row["first"] else broken).append(line)
    return fixed, broken


def main():
    head = load(Path(sys.argv[1]))
    base = load(Path(sys.argv[2])) if len(sys.argv) > 2 else {}
    out = ["## `commands search` eval", ""]
    if not head:
        out.append("No reports were written; the eval tests did not run.")
        print("\n".join(out))
        return
    if not base:
        out += ["The base branch has no eval reports, so only this branch's scores are shown.", ""]

    out += [
        "| Set | Rows | First | Top 3 | Floor |",
        "| --- | --- | --- | --- | --- |",
    ]
    details = []
    for name, (floors, rows) in head.items():
        base_rows = base.get(name, (None, None))[1]
        first, top3 = score(rows, "first"), score(rows, "top3")
        base_first = score(base_rows, "first") if base_rows else None
        base_top3 = score(base_rows, "top3") if base_rows else None
        floor = f"{floors[0]} / {floors[1]}" if floors else "-"
        below = floors and (first < floors[0] or top3 < floors[1])
        out.append(
            f"| `{name}` | {len(rows)} | {delta(first, base_first)} | "
            f"{delta(top3, base_top3)} | {floor}{' ⚠️ below' if below else ''} |"
        )

        if base_rows:
            fixed, broken = changed(rows, base_rows)
            header = ["| Query | Want | Base | This branch |", "| --- | --- | --- | --- |"]
            if broken:
                details += [f"### `{name}`: {len(broken)} no longer first", ""] + header + broken + [""]
            if fixed:
                details += [f"### `{name}`: {len(fixed)} now first", ""] + header + fixed + [""]

        misses = [
            f"| {query} | {row['expected']} | {row['found']} |"
            for query, row in rows.items()
            if not row["first"]
        ]
        if misses:
            details += [
                f"<details><summary><code>{name}</code>: {len(misses)} not first</summary>",
                "",
                "| Query | Want | Got (top 3) |",
                "| --- | --- | --- |",
                *misses,
                "",
                "</details>",
                "",
            ]

    out += ["", *details]
    print("\n".join(out))


if __name__ == "__main__":
    main()
