#!/bin/sh
# Checks that `scripts/commands-search-report.py` says what happened for each
# way the CI `commands search eval` job can go, from hand-written reports.
#
# The job promises never to fail, and its summary is the only place a
# reviewer sees the result, so a summary that is wrong is worse than none:
# the cases here are the ones it once got wrong or could not tell apart. A
# base whose run failed, a push with no base and a base from before the eval
# all used to read "the base branch has no eval reports"; a malformed line
# raised a traceback and turned the job red.
#
# No Rust and no network: the reports are written here in the format the
# `eval_set_*` tests write.
#
# Usage:
#   sh scripts/test-commands-search-report.sh

set -eu

SCRIPT="$(dirname "$0")/commands-search-report.py"
PYTHON="${PYTHON:-python3}"

failures=0

say() {
    printf '%s\n' "$*"
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}

WORK="$(mktemp -d)"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# One report: a floors line, then each argument as a row.
report() {
    file="$1"
    shift
    mkdir -p "$(dirname "$file")"
    printf '# floors\t1\t1\n' >"$file"
    for row in "$@"; do
        printf '%s\n' "$row" >>"$file"
    done
}

TAB="$(printf '\t')"
row() {
    printf '%s' "$1${TAB}$2${TAB}$3${TAB}$4${TAB}$5"
}

# Runs the script, which must exit 0, into `$WORK/out`.
summarize() {
    if ! "$PYTHON" "$SCRIPT" "$@" >"$WORK/out" 2>"$WORK/err"; then
        fail "exited non-zero for: $*: $(cat "$WORK/err")"
        return 1
    fi
    if [ -s "$WORK/err" ]; then
        fail "wrote to stderr for: $*: $(cat "$WORK/err")"
    fi
}

expect() {
    case_name="$1"
    text="$2"
    if grep -qF -- "$text" "$WORK/out"; then
        say "ok: $case_name"
    else
        fail "$case_name: no \"$text\" in:"
        sed 's/^/    /' "$WORK/out" >&2
    fi
}

reject() {
    case_name="$1"
    text="$2"
    if grep -qF -- "$text" "$WORK/out"; then
        fail "$case_name: unexpected \"$text\" in:"
        sed 's/^/    /' "$WORK/out" >&2
    else
        say "ok: $case_name"
    fi
}

SETS="$WORK/sets"
mkdir -p "$SETS"
: >"$SETS/a.tsv"
: >"$SETS/b.tsv"

HEAD="$WORK/head"
report "$HEAD/a.tsv" \
    "$(row 'upload a font' 'mapbox fonts upload' 'mapbox fonts upload' 1 1)" \
    "$(row 'a | b query' 'mapbox x|mapbox y' 'mapbox z' 0 0)"
report "$HEAD/b.tsv" \
    "$(row 'list styles' 'mapbox styles list' 'mapbox styles list' 1 1)"

BASE="$WORK/base"
report "$BASE/a.tsv" \
    "$(row 'upload a font' 'mapbox fonts upload' 'mapbox fonts list' 0 1)" \
    "$(row 'a | b query' 'mapbox x|mapbox y' 'mapbox z' 0 0)"

if summarize --head "$HEAD" --head-status 0 --base "$BASE" --base-status 0 --sets "$SETS"; then
    expect "a fix against the base is listed" "1 now first"
    expect "a set new in this branch is marked" "only in this branch"
    expect "a pipe in a cell is escaped" 'a \| b query'
    expect "a score carries its share and its change" "| 1 (50%, +1) |"
    expect "an unchanged score carries its share alone" "| 1 (50%) |"
    expect "the columns are named" "| Query set | Queries | Ranked first | In top 3 |"
    expect "the legend says what the columns mean" "**Ranked first**: the expected command"
    reject "no emoji" "⚠"
fi

if summarize --head "$HEAD" --head-status 0 --sets "$SETS"; then
    expect "a push has no base" "Not a pull request"
fi

if summarize --head "$HEAD" --head-status 0 --base "$WORK/none" --base-status 0 --sets "$SETS"; then
    expect "a base from before the eval" "The base predates the eval"
fi

if summarize --head "$HEAD" --head-status 0 --base "$WORK/none" --base-status 101 --sets "$SETS"; then
    expect "a base whose run failed" "The base run failed (exit 101)"
fi

if summarize --head "$HEAD" --head-status 0 --base "$WORK/none" --base-status '' --sets "$SETS"; then
    expect "a base whose step never recorded a status" "The base run did not finish"
fi

ONLY_A="$WORK/only-a"
report "$ONLY_A/a.tsv" "$(row 'q' 'mapbox x' 'mapbox x' 1 1)"
if summarize --head "$ONLY_A" --head-status 101 --base "$BASE" --base-status 0 --sets "$SETS"; then
    expect "a set missing from this run is named" "missing from this run"
    expect "a failing head run says so" "failed (exit 101)"
fi

if summarize --head "$ONLY_A" --head-status 0 --base "$HEAD" --base-status 0; then
    expect "a set only in the base is marked" "only in the base"
fi

if summarize --head "$WORK/none" --head-status 101; then
    expect "no reports and a failed run" "before writing any report"
fi

BAD="$WORK/bad"
report "$BAD/a.tsv" \
    "$(row 'fine' 'mapbox x' 'mapbox x' 1 1)" \
    "too${TAB}few${TAB}columns" \
    "$(row 'odd flags' 'mapbox x' 'mapbox x' yes 1)"
printf '# floors\tnot-a-number\n' >>"$BAD/a.tsv"
printf '# invalid\tmapbox gone is not a command search answers with ("q")\n' >>"$BAD/a.tsv"
printf '\377\376 not utf-8\n' >"$BAD/b.tsv"
if summarize --head "$BAD" --head-status 101 --sets "$SETS"; then
    expect "malformed lines are skipped and counted" "3 malformed lines skipped"
    expect "an invalid row is listed" "mapbox gone"
    expect "an unreadable report is named" "unreadable"
fi

# The PR comment: `drop` only past the threshold, `ok` when compared and
# below it, `none` when there was nothing to compare against.
# Four rows each; a "won" row ranks first, a "lost" one does not.
won() { row "$1" 'mapbox x' 'mapbox x' 1 1; }
lost() { row "$1" 'mapbox x' 'mapbox y' 0 0; }
report "$WORK/cbase/a.tsv" "$(won q1)" "$(won q2)" "$(won q3)" "$(won q4)"
report "$WORK/cdrop/a.tsv" "$(lost q1)" "$(lost q2)" "$(lost q3)" "$(won q4)"
report "$WORK/cslip/a.tsv" "$(won q1)" "$(won q2)" "$(won q3)" "$(won q4)"

check_state() {
    case_name="$1"
    want="$2"
    got="$(cat "$WORK/comment/state" 2>/dev/null || echo missing)"
    if [ "$got" = "$want" ]; then
        say "ok: $case_name"
    else
        fail "$case_name: state is $got, not $want"
    fi
}

if summarize --head "$WORK/cdrop" --head-status 0 --base "$WORK/cbase" --base-status 0 \
    --comment-dir "$WORK/comment"; then
    check_state "three queries lost is a drop" drop
    cp "$WORK/comment/body.md" "$WORK/out"
    expect "the comment names the set and the change" "| \`a.tsv\` | 4 (100%) | 1 (25%) | -3 queries, -75 points |"
    expect "the comment lists what is no longer first" "3 no longer first"
fi
rm -rf "$WORK/comment"

if summarize --head "$WORK/cslip" --head-status 0 --base "$WORK/cbase" --base-status 0 \
    --comment-dir "$WORK/comment"; then
    check_state "no loss is ok" ok
    if [ -e "$WORK/comment/body.md" ]; then
        fail "an ok state wrote a comment body"
    else
        say "ok: an ok state writes no body"
    fi
fi
rm -rf "$WORK/comment"

if summarize --head "$WORK/cdrop" --head-status 0 --base "$WORK/cbase" --base-status 101 \
    --comment-dir "$WORK/comment"; then
    check_state "a failed base run is nothing to compare" none
fi
rm -rf "$WORK/comment"

if summarize --head "$WORK/cdrop" --head-status 0 --comment-dir "$WORK/comment"; then
    check_state "no base is nothing to compare" none
fi

if [ "$failures" -ne 0 ]; then
    say "$failures check(s) failed."
    exit 1
fi

say "All checks passed."
