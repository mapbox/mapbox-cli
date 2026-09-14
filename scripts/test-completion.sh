#!/bin/sh
# Checks that `mapbox completion <shell>` produces a script the shell it names
# will actually load — the half of that verification that no Rust test can
# reach, because it needs the shell itself.
#
# `tests/completion.rs` covers what is checkable in `cargo test`: that a
# script is produced for every shell claimed, that it names the services,
# operations and flags this build has, and that a withheld operation is absent
# from all four. What it cannot do is tell a script that parses from one the
# shell accepts, so that is here.
#
# Two tiers, and the difference is deliberate rather than an omission:
#
#   * **Loads** — all four shells, in a fresh session with no user config
#     (`bash --noprofile`, `zsh -f`, `fish --no-config`, `pwsh -NoProfile`),
#     each the way that shell really installs a completion: bash and fish
#     source the file, PowerShell dot-sources it, and zsh is given the
#     directory on `$fpath` and left to find `_mapbox` through `compinit`. A
#     syntax error, a builtin used wrongly, or a `#compdef` line zsh will not
#     register all fail here.
#   * **Completes** — bash and fish. These two can be driven with no terminal
#     attached: bash's completion function is callable directly with
#     `COMP_WORDS`/`COMP_CWORD` set, and fish has `complete -C`. So for those
#     two the check is the real thing — a service, an operation and a flag are
#     completed from a partial command line. zsh and PowerShell drive their
#     completers only through a pty, which is a test harness rather than a
#     script; for them the load check plus the content assertions in
#     `tests/completion.rs` are what is claimed.
#
# A shell that is not installed is skipped and named in the summary. That is
# the honest answer for a laptop and for a runner image alike — the alternative
# is a check that quietly passes because it ran nothing.
#
# Usage:
#   sh scripts/test-completion.sh              # target/debug/mapbox
#   MAPBOX_BIN=/path/to/mapbox sh scripts/test-completion.sh

set -eu

# The partial command lines the "completes" tier types, and what each has to
# offer. Chosen for what they are rather than for being convenient: a service
# (a top-level subcommand), an operation (a subcommand of one) and a flag of
# that operation.
SERVICE_PREFIX='sty'
SERVICE_EXPECT='styles'
OPERATION_PREFIX='lis'
OPERATION_EXPECT='list'
FLAG_PREFIX='--dr'
FLAG_EXPECT='--draft'

failures=0
skipped=''
checked=''

say() {
    printf '%s\n' "$*"
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}

pass() {
    printf 'ok: %s\n' "$*"
}

# The binary under test. `.exe` because this runs on a Windows runner too,
# under git-bash, where cargo writes `mapbox.exe`.
find_binary() {
    if [ -n "${MAPBOX_BIN:-}" ]; then
        printf '%s' "$MAPBOX_BIN"
        return 0
    fi
    for candidate in target/debug/mapbox target/debug/mapbox.exe \
        target/release/mapbox target/release/mapbox.exe; do
        if [ -x "$candidate" ]; then
            printf '%s' "$candidate"
            return 0
        fi
    done
    return 1
}

if ! BIN="$(find_binary)"; then
    say "No mapbox binary found. Run \`cargo build\` first, or set MAPBOX_BIN."
    exit 1
fi
# An absolute path: every check below runs the shell from a scratch directory.
case "$BIN" in
/*) ;;
*) BIN="$(pwd)/$BIN" ;;
esac

WORK="$(mktemp -d)"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

say "Binary: $BIN"
say "$("$BIN" --version)"
say ""

# Writes one shell's script to `script_for "$shell"`, and returns non-zero
# having already reported why if it could not.
#
# The path is derived rather than echoed back, deliberately: an earlier
# version returned it on stdout, which meant every call sat inside a command
# substitution — a subshell, where `fail`'s increment of `$failures` is
# discarded when it exits. The suite then reported four failures and exited 0.
generate() {
    shell="$1"
    out="$(script_for "$shell")"
    if ! "$BIN" completion "$shell" >"$out" 2>"$WORK/gen.err"; then
        fail "$shell: \`mapbox completion $shell\` exited non-zero: $(cat "$WORK/gen.err")"
        return 1
    fi
    if [ ! -s "$out" ]; then
        fail "$shell: \`mapbox completion $shell\` wrote nothing"
        return 1
    fi
    return 0
}

# Each script is written under the name that shell installs it as, which is
# not cosmetic in two of the four cases. PowerShell dot-sources a `.ps1` and
# tries to *execute* anything else as an external program, and zsh's
# `compinit` finds a completion by looking for a file called `_<command>` on
# `$fpath` — so naming them by shell instead would test something neither
# shell will ever be asked to do.
script_for() {
    case "$1" in
    bash) printf '%s' "$WORK/mapbox.bash" ;;
    zsh) printf '%s' "$WORK/_mapbox" ;;
    fish) printf '%s' "$WORK/mapbox.fish" ;;
    powershell) printf '%s' "$WORK/mapbox.ps1" ;;
    *) printf '%s' "$WORK/mapbox.$1" ;;
    esac
}

# The same path as a native Windows one, where that is a different thing.
#
# This script runs under git-bash on the Windows runner, where `mktemp -d`
# answers `/tmp/tmp.XXXX` — a path only MSYS understands. MSYS rewrites
# arguments that look like paths on their way to a native program, which is
# why `pwsh -File /tmp/…/check.ps1` finds the file; it does not rewrite the
# *contents* of that file, so a path written into the script it dot-sources
# reaches PowerShell untranslated and resolves to nothing. `cygpath` is what
# knows the mapping, and it exists only where the problem does.
win_path() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$1"
    else
        printf '%s' "$1"
    fi
}

# How one long flag is written in one shell's script.
#
# bash, zsh and PowerShell all carry the flag as typed. fish's `complete`
# takes it as `-l draft`, with the dashes supplied by fish itself, so looking
# for `--draft` there finds nothing and reports a script that is perfectly
# correct as broken.
flag_in() {
    case "$1" in
    fish) printf -- '-l %s' "$(printf '%s' "$2" | sed 's/^-*//')" ;;
    *) printf '%s' "$2" ;;
    esac
}

# The three names have to be in the script at all before asking a shell to
# offer them — a script that loads cleanly and completes nothing would
# otherwise pass the load tier in silence.
check_content() {
    shell="$1"
    script="$2"
    for needle in "$SERVICE_EXPECT" "$OPERATION_EXPECT" "$(flag_in "$shell" "$FLAG_EXPECT")"; do
        if ! grep -q -- "$needle" "$script"; then
            fail "$shell: the script never mentions \`$needle\`"
            return 1
        fi
    done
    pass "$shell: names a service, an operation and a flag"
}

check_bash() {
    script="$1"
    if ! bash -n "$script" 2>"$WORK/bash.err"; then
        fail "bash: the script does not parse: $(cat "$WORK/bash.err")"
        return 1
    fi
    pass "bash: parses"

    # `--noprofile --norc`: what is being tested is the generated script, not
    # whatever bash-completion the machine happens to have loaded.
    if ! bash --noprofile --norc -c ". '$script'; declare -F _mapbox >/dev/null" \
        2>"$WORK/bash.err"; then
        fail "bash: sourcing it does not define _mapbox: $(cat "$WORK/bash.err")"
        return 1
    fi
    pass "bash: sources and defines _mapbox"

    # The real thing: bash's completion function takes (command, cur, prev)
    # and reads COMP_WORDS/COMP_CWORD, all of which can be set by hand with no
    # terminal in sight. This is exactly what bash itself passes.
    bash_completes 'mapbox' "$SERVICE_PREFIX" 'mapbox' "$SERVICE_EXPECT" 'a service'
    bash_completes 'mapbox styles' "$OPERATION_PREFIX" 'styles' "$OPERATION_EXPECT" 'an operation'
    bash_completes 'mapbox styles list' "$FLAG_PREFIX" 'list' "$FLAG_EXPECT" 'a flag'
}

# line: the words already typed, cur: the partial word, prev: the word before
# it, expect: what has to come back, what: how the result is described.
bash_completes() {
    line="$1"
    cur="$2"
    prev="$3"
    expect="$4"
    what="$5"

    got="$(bash --noprofile --norc -c "
        . '$script'
        COMP_WORDS=($line '$cur')
        COMP_CWORD=\$((\${#COMP_WORDS[@]} - 1))
        _mapbox mapbox '$cur' '$prev'
        printf '%s\n' \"\${COMPREPLY[@]}\"
    " 2>"$WORK/bash.err")" || {
        fail "bash: completing \`$line $cur\` errored: $(cat "$WORK/bash.err")"
        return 1
    }

    if printf '%s\n' "$got" | grep -qx -- "$expect"; then
        pass "bash: completes $what (\`$line $cur\` → $expect)"
    else
        fail "bash: \`$line $cur\` did not offer $expect (got: $(printf '%s' "$got" | tr '\n' ' '))"
    fi
}

check_zsh() {
    script="$1"
    if ! zsh -n "$script" 2>"$WORK/zsh.err"; then
        fail "zsh: the script does not parse: $(cat "$WORK/zsh.err")"
        return 1
    fi
    pass "zsh: parses"

    # The real installation path, not an approximation of it: the script is
    # written as `_mapbox` (see `script_for`), its directory goes on `$fpath`,
    # and `compinit` is left to find it. What that proves is the thing a user
    # cares about — zsh's completion system has `mapbox` registered, from the
    # `#compdef mapbox` line at the top of the generated file. Sourcing the
    # file by hand would define the function whether or not zsh would ever
    # have found it.
    #
    # `-f` skips every startup file, so nothing but this script and zsh's own
    # completion system is loaded. `-u` skips the insecure-directory prompt,
    # which a scratch directory would otherwise trip.
    if ! zsh -f -c "
        fpath=('$WORK' \$fpath)
        autoload -Uz compinit
        compinit -u -d '$WORK/zcompdump'
        [[ -n \${_comps[mapbox]} ]] || { print -u2 'compinit did not register mapbox'; exit 1; }
        exit 0
    " 2>"$WORK/zsh.err"; then
        fail "zsh: compinit did not pick the script up off \$fpath: $(cat "$WORK/zsh.err")"
        return 1
    fi
    pass "zsh: installs as _mapbox on \$fpath and compinit registers it"
}

check_fish() {
    script="$1"
    # fish's own parser check, the equivalent of `bash -n`.
    if ! fish --no-execute "$script" 2>"$WORK/fish.err"; then
        fail "fish: the script does not parse: $(cat "$WORK/fish.err")"
        return 1
    fi
    pass "fish: parses"

    if ! fish --no-config --command "source '$script'" 2>"$WORK/fish.err"; then
        fail "fish: sourcing it failed: $(cat "$WORK/fish.err")"
        return 1
    fi
    pass "fish: sources"

    # `complete -C` asks fish for the completions of a command line, with no
    # terminal and no keystroke. Its output is one candidate per line, each
    # optionally followed by a tab and a description.
    fish_completes "mapbox $SERVICE_PREFIX" "$SERVICE_EXPECT" 'a service'
    fish_completes "mapbox styles $OPERATION_PREFIX" "$OPERATION_EXPECT" 'an operation'
    fish_completes "mapbox styles list $FLAG_PREFIX" "$FLAG_EXPECT" 'a flag'
}

fish_completes() {
    line="$1"
    expect="$2"
    what="$3"

    got="$(fish --no-config --command "source '$script'; complete -C '$line'" \
        2>"$WORK/fish.err")" || {
        fail "fish: completing \`$line\` errored: $(cat "$WORK/fish.err")"
        return 1
    }

    if printf '%s\n' "$got" | cut -f1 | grep -qx -- "$expect"; then
        pass "fish: completes $what (\`$line\` → $expect)"
    else
        fail "fish: \`$line\` did not offer $expect (got: $(printf '%s' "$got" | tr '\n' ' '))"
    fi
}

check_powershell() {
    script="$1"
    # Written to a file and run with `-File` rather than passed to
    # `-Command`. A multi-line `-Command` string is read as console input,
    # and a pwsh with nothing on stdin waits for more of it — which is a
    # hang, not a failure, and a hang in CI costs the job's whole timeout.
    # `-File` runs a script and exits.
    # Dot-sourcing is the whole check, and it is enough: PowerShell parses the
    # file before running any of it, so a syntax error fails here with its own
    # line and column, and what runs is `Register-ArgumentCompleter` — the
    # half that decides whether the script does anything once loaded.
    #
    # An explicit `Parser::ParseFile` pass ahead of it was tried and dropped.
    # It reported the same errors the dot-source already reports, and on a
    # 280 KB script it is the one call heavy enough to matter.
    #
    # The path inside the checker goes through `win_path`, because on the
    # Windows runner this shell's idea of a path and PowerShell's are not the
    # same string. Both are, for symmetry and so neither depends on MSYS
    # rewriting an argument on the way past.
    checker="$WORK/check.ps1"
    cat >"$checker" <<PS1
\$ErrorActionPreference = 'Stop'
. '$(win_path "$script")'
exit 0
PS1

    if ! "$PWSH" -NoProfile -NonInteractive -File "$(win_path "$checker")" \
        >"$WORK/pwsh.out" 2>"$WORK/pwsh.err" </dev/null; then
        fail "powershell: the script does not dot-source: $(cat "$WORK/pwsh.err")"
        return 1
    fi
    pass "powershell: dot-sources and registers its completer"
}

run_shell() {
    shell="$1"
    # The executable to look for. It is the shell's own name for three of
    # them, and `pwsh` for the fourth: this runs under git-bash on Windows,
    # where `powershell.exe` is Windows PowerShell — a different program from
    # the one anyone installs completions into today.
    tool="$2"

    if ! command -v "$tool" >/dev/null 2>&1; then
        say "-- $shell: $tool not installed, skipped"
        skipped="$skipped $shell"
        return 0
    fi

    say "-- $shell ($(command -v "$tool"))"
    if generate "$shell"; then
        script="$(script_for "$shell")"
        if check_content "$shell" "$script"; then
            "check_$shell" "$script" || true
        fi
    fi
    checked="$checked $shell"
    say ""
}

PWSH=pwsh
run_shell bash bash
run_shell zsh zsh
run_shell fish fish
run_shell powershell "$PWSH"

say "Checked:${checked:- none}"
say "Skipped:${skipped:- none}"

if [ "$failures" -ne 0 ]; then
    say "$failures check(s) failed."
    exit 1
fi

say "All checks passed."
