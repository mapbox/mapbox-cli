#!/bin/sh
# Installs the `mapbox` CLI: resolve the target from uname, read the channel
# manifest, download the artifact it names, verify its SHA-256, move the binary
# into place — then offer to set up the Tilesets CLI that `mapbox tilesets-cli`
# proxies to.
#
# Three constraints that are easy to break:
#
#   * POSIX sh, not bash. This is piped into `sh` on machines we do not
#     control, so no [[ ]], no arrays, no `local`. `shellcheck -s sh` and
#     scripts/test-install.sh run in CI to keep it honest.
#   * It is served, not run from the repo. Mapbox's release pipeline
#     substitutes __MAPBOX_CLI_BASE_URL__ for the channel's own URL before
#     uploading this file, so anything new that differs per channel has to go
#     through the same substitution. A copy with nothing substituted in stops
#     at the guard below rather than trying to download from a placeholder.
#   * Nothing here may read stdin. Under `curl ... | sh` stdin *is* this
#     script, so a `read` would eat the rest of it. The one prompt reads
#     /dev/tty, and every child process is started with </dev/null.
set -eu

BASE_URL="${MAPBOX_CLI_BASE_URL:-__MAPBOX_CLI_BASE_URL__}"
VERSION="${MAPBOX_CLI_VERSION:-latest}"

# A pinned version is accepted with or without the leading `v`.
#
# The channel's directories are named `v0.2.1`, but every place a person reads
# a version from shows it without one: `mapbox --version`, CHANGELOG.md,
# Cargo.toml. So the spelling somebody copies is the spelling that used to
# fail, and it failed as `403` from S3 on a path that does not exist — which
# reads as "you are not allowed" rather than "no such version".
#
# `latest` and anything else non-numeric is left alone: only a leading digit
# means a version number is being named.
case "$VERSION" in
    [0-9]*) VERSION="v${VERSION}" ;;
esac
INSTALL_DIR="${MAPBOX_INSTALL_DIR:-$HOME/.local/bin}"

# Where a reader is sent to build from source or report a bad artifact. One
# variable rather than three literals, the way install.ps1 keeps its $Repo:
# this script is served on its own, so a repository rename has to be a single
# edit here and a single edit there.
REPO='https://github.com/mapbox/mapbox-cli'

# yes or no to answer the Tilesets CLI prompt ahead of time. Unset means ask
# when there is a terminal, and no when there isn't.
INSTALL_TILESETS="${MAPBOX_INSTALL_TILESETS:-}"

# The install dir goes on PATH through the shell's profile unless this is set,
# the same switch, read the same way, as install.ps1's registry edit: a fresh
# Mac has no ~/.local/bin on PATH, so without it nearly every first install
# ends with a `command not found`.
MODIFY_PATH=yes
case "${MAPBOX_NO_MODIFY_PATH:-}" in
    '' | 0 | no | false) ;;
    *) MODIFY_PATH=no ;;
esac

# Only set if you need a non-production channel. Export
# MAPBOX_CLI_AUTH=user:password to authenticate to it — the same credential
# the outer `curl -u` used to fetch this script, which is why it has to be
# exported rather than passed on the curl command line only.
AUTH="${MAPBOX_CLI_AUTH:-}"

# Every request below goes out under this name rather than curl's own, so a
# download that came from the installer can be told apart from a hand-written
# curl, a mirror, or CI pulling the same tarball — the User-Agent is what the
# access logs carry either way. It says nothing about who is installing: the
# channel and the artifact are in the request path already, and the target
# triple appended further down is what `uname` reports.
#
# This string and install.ps1's are the only two copies of it, because each
# script is downloaded and run on its own and can read nothing else. They are
# not left to agree by hand: test-install.sh reads both and fails when they
# differ, and both test suites take what they assert from the installer rather
# than writing it down a third time. So bump it here, bump it there, and CI
# says so if you did only one — the failure it prevents is two shapes in the
# logs with nothing to say which installer sent which.
USER_AGENT='mapbox-cli-install/1'

# Set MAPBOX_CLI_INSTALL_SOURCE to name what is doing the installing — a
# Dockerfile, an onboarding script, a channel smoke test — and it rides along
# as `src/<value>`. Everything but letters, digits, dot, dash and underscore is
# dropped, because this value comes from the environment and ends up in a
# header.
#
# **Convention for a coding agent invoking this script on someone's behalf:**
# `agent-<name>` — `agent-claude-code`, `agent-cursor` — so an access-log query
# can tell an agent-driven install from a human or CI one without a second
# reporting mechanism. A dash, not a slash: `/` is one of the characters the
# sanitizer above strips, so `agent/claude-code` would silently become
# `agentclaude-code` and lose the separator that makes the convention legible.
# Freeform otherwise, the same as every other value here — no fixed list of
# agent names to keep in sync.
INSTALL_SOURCE="${MAPBOX_CLI_INSTALL_SOURCE:-}"

# The switch `src/telemetry.rs` honors for the CLI's own User-Agent, read here
# the same way, because someone who put it in a Dockerfile and then pipes this
# script into sh in the same file has already said which way they want it. The
# product token above is what survives it — the equivalent of
# `mapbox-cli/<version>` going out either way — and everything appended below
# is what it drops.
#
# **Two names, and only the binary dropped the old one.**
# `MAPBOX_CLI_NO_TELEMETRY` is the documented switch; `DISABLE_TELEMETRY` is
# what it was called before, and this script still honors it. The rename was
# announced as breaking for the binary, so a `DISABLE_TELEMETRY=1` there
# genuinely stopped working and the changelog says so. Nothing announced it for
# the installers — this file is fetched and run in one line, so a reader has no
# release notes in front of them — and breaking an opt-out is the one change
# that must not happen quietly. So both work here, the new name wins when both
# are set, and the old one keeps working for the Dockerfile the comment above
# describes.
#
# Unset, empty, or whitespace: a cleared variable. `0`, `f`, `false`, `n`, `no`
# and `off` are clap's false spellings, so a `0` is someone declining the
# opt-out rather than taking it. Anything else opts out, including a spelling
# nobody planned for: the safe reading of a value we do not know, on a variable
# by that name, is the one that sends less.
telemetry_allowed() {
    # Set at all — even to empty, which is how a shell clears one — means the
    # new name is the answer. Otherwise fall back, so the two never have to be
    # reconciled: an explicit `MAPBOX_CLI_NO_TELEMETRY=0` beats a stale
    # `DISABLE_TELEMETRY=1` left in an image from before the rename.
    if [ -n "${MAPBOX_CLI_NO_TELEMETRY+set}" ]; then
        _telemetry_switch=$MAPBOX_CLI_NO_TELEMETRY
    else
        _telemetry_switch=${DISABLE_TELEMETRY-}
    fi

    # ASCII-only on purpose, the way `to_ascii_lowercase` is in
    # src/telemetry.rs: [:upper:]/[:lower:] would bring the locale into a
    # decision about six ASCII spellings, and this script sets no LC_ALL.
    # shellcheck disable=SC2018,SC2019
    case "$(printf '%s' "$_telemetry_switch" |
        tr 'A-Z' 'a-z' |
        sed 's/^[[:space:]]*//;s/[[:space:]]*$//')" in
        '' | 0 | f | false | n | no | off) return 0 ;;
        *) return 1 ;;
    esac
}

fetch() {
    if [ -n "$AUTH" ]; then
        curl -fsSL -A "$USER_AGENT" -u "$AUTH" "$@"
    else
        curl -fsSL -A "$USER_AGENT" "$@"
    fi
}

# --- Presentation ----------------------------------------------------------
#
# Color under the rules src/output/style.rs applies to the CLI itself: a
# terminal, no NO_COLOR (empty counts as unset), and not TERM=dumb. The palette
# is the terminal's own, so it reads on light and dark backgrounds alike.
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ]; then
    BOLD="$(printf '\033[1m')"
    DIM="$(printf '\033[2m')"
    GREEN="$(printf '\033[32m')"
    YELLOW="$(printf '\033[33m')"
    RED="$(printf '\033[31m')"
    ACCENT="$(printf '\033[94m')"
    RESET="$(printf '\033[0m')"
else
    BOLD='' DIM='' GREEN='' YELLOW='' RED='' ACCENT='' RESET=''
fi

# A check mark in a C locale prints as mojibake, so fall back to ASCII unless
# the locale says UTF-8.
case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
    *UTF-8* | *utf-8* | *UTF8* | *utf8*)
        MARK_OK='✓' MARK_SKIP='–' MARK_WARN='!' MARK_ASK='?' MARK_WAIT='…'
        SPIN_FRAMES='⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏'
        ;;
    *)
        MARK_OK='*' MARK_SKIP='-' MARK_WARN='!' MARK_ASK='?' MARK_WAIT='.'
        SPIN_FRAMES='- \\ | /'
        ;;
esac

# The install log (see "Install log" below), set once it can be written.
INSTALL_LOG=''
ESC="$(printf '\033')"
# Appends to the install log, without color codes. Never fails the install.
log() {
    [ -n "$INSTALL_LOG" ] || return 0
    printf '%s\n' "$*" | sed "s/${ESC}\\[[0-9;]*m//g" >>"$INSTALL_LOG" 2>/dev/null || true
}

# Where a step line starts. A question's answers are its children, so a step
# that asked one sets this two deeper while it reports, then puts it back.
STEP_INDENT='  '

step_ok() { printf '%s%s%s%s %s\n' "$STEP_INDENT" "$GREEN" "$MARK_OK" "$RESET" "$*"; log "ok: $*"; }
step_skip() { printf '%s%s%s%s %s\n' "$STEP_INDENT" "$DIM" "$MARK_SKIP" "$RESET" "$*"; log "skip: $*"; }
step_warn() { printf '%s%s%s%s %s\n' "$STEP_INDENT" "$YELLOW" "$MARK_WARN" "$RESET" "$*"; log "warn: $*"; }
# A line of detail under the step line before it, aligned with its text.
step_detail() { printf '%s  %s\n' "$STEP_INDENT" "$*"; log "  $*"; }

# Runs a command for its exit status only; what it prints goes to the log.
log_run() { # command...
    log "\$ $*"
    if [ -n "$INSTALL_LOG" ]; then
        if "$@" </dev/null >>"$INSTALL_LOG" 2>&1; then lr_status=0; else lr_status=$?; fi
    else
        if "$@" </dev/null >/dev/null 2>&1; then lr_status=0; else lr_status=$?; fi
    fi
    log "(exit ${lr_status})"
    return "$lr_status"
}

# Runs a command for its stdout, which it prints for the caller to capture;
# stdout and stderr both go to the log as well.
log_capture() { # command...
    log "\$ $*"
    lc_err="${WORK_DIR}/log-capture.err"
    if lc_out="$("$@" </dev/null 2>"$lc_err")"; then lc_status=0; else lc_status=$?; fi
    [ -z "$lc_out" ] || log "$lc_out"
    if [ -s "$lc_err" ]; then log "$(cat "$lc_err")"; fi
    log "(exit ${lc_status})"
    printf '%s' "$lc_out"
    return "$lc_status"
}

# A question reads in three layers: the question itself in bold, what
# answering it does dimmed under it, and the command it runs in the accent
# color, so the eye lands on the question and the command and can skip the
# rest.
ask_title() { printf '  %s%s%s%s %s%s%s\n' "$ACCENT" "$BOLD" "$MARK_ASK" "$RESET" "$BOLD" "$*" "$RESET"; }
ask_note() { printf '    %s%s%s\n' "$DIM" "$*" "$RESET"; }
# Inside an ask_note: the command in the accent color, then back to dim.
ask_cmd() { printf '%s%s%s%s' "$RESET" "$ACCENT" "$1" "$RESET$DIM"; }

# Runs a command that can take a while and talks while it does (pipx, pip, a
# coding agent's own `mcp add`). At a terminal, a spinner and the label sit on
# one line with the command's latest output dimmed under it, both erased when
# it ends; elsewhere the label is printed once. Everything goes to <log>
# either way, for a caller to show in full when it is the diagnosis.
# <stdout-file>, when not empty, takes the command's stdout instead, for a
# caller that parses it.
# The child's own colors and control characters would throw off both the
# width and the redraw, so only its text is shown.
run_live_text() {
    sed "s/${ESC}\\[[0-9;?]*[A-Za-z]//g" | tr '\t' ' ' | tr -d '\000-\010\013-\037\177'
}

# A line with a URL in it is usually one to open — a sign-in page — and the
# spinner would cut it to the screen and erase it at the end. So each new one
# is printed in full above the spinner, with line wrap back on for it, and
# stays there.
run_live_urls() {
    run_total="$(wc -l <"$run_log" 2>/dev/null | tr -d ' ')"
    [ -n "$run_total" ] && [ "$run_total" -gt "$run_seen" ] || return 0
    sed -n "$((run_seen + 1)),${run_total}p" "$run_log" | grep -E 'https?://' | run_live_text |
        while IFS= read -r run_url; do
            printf '\r\033[2K\n\033[2K\033[1A\r\033[?7h%s  %s\n\033[?7l' "$STEP_INDENT" "$run_url"
        done
    run_seen="$run_total"
}

# One frame of run_live: the spinner and label, then the log's latest line.
run_live_draw() {
    run_live_urls
    run_line="$(tail -n 1 "$run_log" 2>/dev/null | run_live_text | cut -c "1-${run_width}")"
    printf '\r\033[2K%s%s%s%s %s\n\033[2K%s  %s%s%s\033[1A\r' \
        "$STEP_INDENT" "$ACCENT" "$run_frame" "$RESET" "$run_label" "$STEP_INDENT" "$DIM" "$run_line" "$RESET"
}

run_live() { # label log stdout-file command...
    run_label="$1" run_log="$2" run_stdout="$3"
    shift 3
    : >"$run_log"
    [ -n "$run_stdout" ] || run_stdout="$run_log"
    # A dumb terminal would print the cursor movement as text, every tick.
    if [ ! -t 1 ] || [ "${TERM:-}" = dumb ]; then
        printf '%s%s %s\n' "$STEP_INDENT" "$MARK_WAIT" "$run_label"
        if [ "$run_stdout" = "$run_log" ]; then
            if "$@" </dev/null >>"$run_log" 2>&1; then run_status=0; else run_status=$?; fi
        else
            if "$@" </dev/null >"$run_stdout" 2>>"$run_log"; then run_status=0; else run_status=$?; fi
        fi
        run_live_log "$@"
        return "$run_status"
    fi

    # A line that wraps can no longer be redrawn in place — the spinner moves
    # up one line to start each frame, so a wrapped one pushes every frame
    # after it a line further down. So the log line is cut to what fits after
    # its indent, and line wrap is off while the spinner runs, which also
    # covers a character drawn two columns wide (pipx's emoji) that a cut by
    # character count lets through.
    run_width="$(stty size </dev/tty 2>/dev/null | cut -d' ' -f2)"
    # A pty with no size set, such as script(1)'s, reports 0: unknown, not narrow.
    case "$run_width" in
        '' | 0 | *[!0-9]*) run_width=80 ;;
    esac
    run_width=$((run_width - ${#STEP_INDENT} - 3))
    [ "$run_width" -ge 20 ] || run_width=20
    # A sleep without fractions would make 0.1 an error and the loop a busy
    # one, so fall back to whole seconds.
    run_delay=0.1
    sleep "$run_delay" 2>/dev/null || run_delay=1

    # One writer for both lines: the spinner redraws them from the log until
    # the flag file goes, which cleanup also removes on an interrupt.
    run_flag="${WORK_DIR}/run-live.running"
    : >"$run_flag"
    # The spinner parks the cursor at the start of its line, where a visible
    # one sits on top of the indent, so it is hidden too.
    printf '\033[?25l\033[?7l'
    TERM_MODES_CHANGED=yes
    (
        run_frame="${SPIN_FRAMES%% *}"
        run_seen=0
        while [ -f "$run_flag" ]; do
            # Word splitting is the point: one frame per word.
            # shellcheck disable=SC2086
            for run_frame in $SPIN_FRAMES; do
                [ -f "$run_flag" ] || break
                run_live_draw
                sleep "$run_delay"
            done
        done
        # Once more after the command ends, so output from one that finished
        # between two ticks is still drawn rather than skipped.
        run_live_draw
    ) &
    run_spinner=$!

    if [ "$run_stdout" = "$run_log" ]; then
        if "$@" </dev/null >>"$run_log" 2>&1; then run_status=0; else run_status=$?; fi
    else
        if "$@" </dev/null >"$run_stdout" 2>>"$run_log"; then run_status=0; else run_status=$?; fi
    fi

    rm -f "$run_flag"
    wait "$run_spinner" 2>/dev/null || true
    # Erase the label line and the log line under it.
    printf '\r\033[2K\n\033[2K\033[1A\r\033[?7h\033[?25h'
    TERM_MODES_CHANGED=no
    run_live_log "$@"
    return "$run_status"
}

# What run_live's command printed, and how it ended, into the install log.
run_live_log() { # command...
    log "\$ $*"
    if [ -s "$run_log" ]; then log "$(cat "$run_log")"; fi
    if [ "$run_stdout" != "$run_log" ] && [ -s "$run_stdout" ]; then log "$(cat "$run_stdout")"; fi
    log "(exit ${run_status})"
}

# For display only: a path under $HOME reads shorter as ~/…, and nothing
# shown this way is ever opened.
tildify() {
    # shellcheck disable=SC2088
    case "$1" in
        "$HOME") printf '~' ;;
        "$HOME"/*) printf '~/%s' "${1#"$HOME"/}" ;;
        *) printf '%s' "$1" ;;
    esac
}

die() {
    echo "${RED}mapbox-cli:${RESET} $*" >&2
    log "error: $*"
    exit 1
}

# --- Install log -------------------------------------------------------------
#
# Each run is recorded, so an error seen only in passing — run_live erases a
# command's output once it ends — can be read again afterwards: every step
# line, and every command run after the binary is in place with its full
# output and exit status. The previous run's log is kept beside it as
# install.log.1. Local only: nothing reads or sends it. A directory that cannot
# be written leaves the install unlogged rather than failed.
INSTALL_LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/mapbox-cli"
if mkdir -p "$INSTALL_LOG_DIR" 2>/dev/null; then
    if [ -f "${INSTALL_LOG_DIR}/install.log" ]; then
        mv -f "${INSTALL_LOG_DIR}/install.log" "${INSTALL_LOG_DIR}/install.log.1" 2>/dev/null || true
    fi
    if { printf 'mapbox-cli install.sh, %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" >"${INSTALL_LOG_DIR}/install.log"; } 2>/dev/null; then
        INSTALL_LOG="${INSTALL_LOG_DIR}/install.log"
    fi
fi
log "system: $(uname -srm 2>/dev/null)"
log "install dir: ${INSTALL_DIR}"
log "SHELL: ${SHELL:-}"

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "${os}/${arch}" in
        Darwin/arm64) echo "aarch64-apple-darwin" ;;
        Darwin/x86_64) echo "x86_64-apple-darwin" ;;
        Linux/aarch64 | Linux/arm64) echo "aarch64-unknown-linux-musl" ;;
        Linux/x86_64) echo "x86_64-unknown-linux-musl" ;;
        # Git Bash, MSYS2 and Cygwin each report their own kernel name, and a
        # user reading `no prebuilt binary for MINGW64_NT-10.0-22631` learns
        # nothing from it. This script still cannot be the thing that installs
        # the Windows build: the artifact is a .zip, which Git for Windows' tar
        # will not unpack and which there is no `unzip` here to handle either,
        # and ~/.local/bin plus `chmod 755` is not where a Windows PATH looks.
        # install.ps1 is the thing, and it is served beside this one — so hand
        # the user that line rather than half-installing anything here.
        MINGW* | MSYS* | CYGWIN* | Windows_NT*)
            cat >&2 <<EOF
mapbox-cli: this installer does not run on Windows (${os}).

There is one that does. In PowerShell:

    irm ${BASE_URL}/install.ps1 | iex

EOF
            if [ -n "$AUTH" ]; then
                cat >&2 <<'EOF'
This channel is gated, so that shell needs the credential as well — set it
there before the line above, rather than passing it to irm alone:

    $env:MAPBOX_CLI_AUTH = 'user:password'

EOF
            fi
            cat >&2 <<EOF
Or use WSL, and this script installs the Linux build there unchanged: open a
WSL shell (wsl --install, if you have not already) and run this same command
in it. That is also the only place mapbox tilesets-cli works.

Or build from source: ${REPO}
EOF
            exit 1
            ;;
        *)
            echo "mapbox-cli: no prebuilt binary for ${os} ${arch}." >&2
            echo "Build from source: ${REPO}" >&2
            exit 1
            ;;
    esac
}

# No checksum tool is universal: macOS ships `shasum`, most Linux images ship
# `sha256sum`, and a minimal image may have only `openssl`. Resolve one up
# front, so a download that could not be verified fails before it is made.
CHECKSUM_TOOL=''
for candidate in sha256sum shasum openssl; do
    if command -v "$candidate" >/dev/null 2>&1; then
        CHECKSUM_TOOL="$candidate"
        break
    fi
done

sha256_of() {
    case "$CHECKSUM_TOOL" in
        sha256sum) sha256sum "$1" | cut -d' ' -f1 ;;
        shasum) shasum -a 256 "$1" | cut -d' ' -f1 ;;
        openssl) openssl dgst -sha256 "$1" | sed 's/.*= *//' ;;
    esac
}

for required in curl tar; do
    command -v "$required" >/dev/null 2>&1 ||
        die "${required} is required."
done

[ -n "$CHECKSUM_TOOL" ] ||
    die "no way to compute a SHA-256 here — install one of sha256sum, shasum or openssl."

TARGET="$(detect_target)"

# Not a comparison against the placeholder itself: the release pipeline
# substitutes every occurrence of it in this file, which would rewrite the
# comparison too and make the served copy refuse to install from the channel
# baked into it. Anything that is not a URL is the same problem anyway — and
# without this, a copy run straight from a checkout fails much later and much
# less clearly, as `curl: (6) Could not resolve host`. After the platform
# check rather than before it, the order install.ps1 puts the same two in.
#
# file:// is on the list where install.ps1's `http*` does not need it: a
# channel served out of a directory is what test-install.sh points this at,
# and is how anyone would try a build before publishing it. The Windows
# harness runs a real HTTP listener instead.
case "$BASE_URL" in
    http://* | https://* | file://*) ;;
    *)
        cat >&2 <<EOF
mapbox-cli: no channel to install from.

This is the copy of the installer in the repository, and it has no URL baked
in — the served copies get one substituted at publish time. Install from a
channel:

    curl -fsSL https://cli.mapbox.com/install.sh | sh

Or point this copy at one:

    export MAPBOX_CLI_BASE_URL=https://cli.mapbox.com
EOF
        exit 1
        ;;
esac

# The platform, now that there is one. The manifest request is the only trace
# an install leaves when it never gets as far as an artifact — no build for
# this target, a checksum that did not match — and those are the ones worth
# being able to count separately from the downloads that succeeded.
#
# WSL reports itself as Linux and is otherwise indistinguishable from it,
# which hides it in the one place it matters: a Windows machine that runs this
# script at all is running it there, since detect_target hands the Git Bash /
# MSYS / Cygwin kernels over to install.ps1. The variables are what any
# ordinary shell in WSL has; /proc/sys/kernel/osrelease is the fallback for a
# stripped environment — a provisioner, a systemd unit — and reads
# `microsoft-standard-WSL2`.
if telemetry_allowed; then
    platform="$TARGET"
    case "$TARGET" in
        *-linux-*)
            if [ -n "${WSL_DISTRO_NAME:-}" ] || [ -n "${WSL_INTEROP:-}" ] ||
                grep -qiE 'microsoft|wsl' /proc/sys/kernel/osrelease 2>/dev/null; then
                platform="${TARGET}; wsl"
            fi
            ;;
        # Rosetta is the same blind spot as WSL, pointing the other way: `uname -m`
        # inside a translated process reports x86_64 on an Apple Silicon machine,
        # so an install from a Rosetta shell — or `arch -x86_64 zsh`, or a
        # translated terminal — installs the Intel build and is indistinguishable
        # from a real Intel Mac afterwards. That inflates the Intel share and
        # understates Apple Silicon, in the one measurement anyone would use to
        # decide when the Intel build can be dropped. `sysctl.proc_translated` is
        # what knows: 1 translated, 0 native, and absent on macOS old enough not to
        # have it, where no answer is the right answer. This can only ever fire for
        # the x86_64 triple, since a translated process is what reports x86_64.
        *-apple-darwin)
            if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                platform="${TARGET}; rosetta"
            fi
            ;;
    esac
    USER_AGENT="${USER_AGENT} (${platform})"
    if [ -n "$INSTALL_SOURCE" ]; then
        INSTALL_SOURCE="$(printf '%s' "$INSTALL_SOURCE" | tr -cd 'A-Za-z0-9._-')"
        [ -z "$INSTALL_SOURCE" ] || USER_AGENT="${USER_AGENT} src/${INSTALL_SOURCE}"
    fi
fi

MANIFEST_URL="${BASE_URL}/${VERSION}/manifest.json"

manifest="$(fetch "$MANIFEST_URL")" || {
    echo "mapbox-cli: could not read ${MANIFEST_URL}" >&2
    # Ask again for the status alone rather than reading it off curl's exit
    # code, which is 56 and not 22 for a 401 over HTTP/2. One extra request,
    # only ever on the failure path.
    if [ -z "$AUTH" ]; then
        code="$(curl -s -o /dev/null -w '%{http_code}' -A "$USER_AGENT" "$MANIFEST_URL" 2>/dev/null || true)"
        if [ "$code" = "401" ]; then
            echo "mapbox-cli: this channel is private — export MAPBOX_CLI_AUTH=user:password" >&2
        fi
    fi
    exit 1
}

# Deliberately not jq — the installer must run on a bare machine.
field() {
    printf '%s' "$manifest" |
        tr -d '\n ' |
        sed -n "s/.*\"${TARGET}\":{[^}]*\"$1\":\"\([^\"]*\)\".*/\1/p"
}

resolved_version="$(printf '%s' "$manifest" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
file="$(field file)"
sha256="$(field sha256)"

if [ -z "$file" ]; then
    echo "mapbox-cli: ${MANIFEST_URL} lists no artifact for ${TARGET}." >&2
    exit 1
fi

# The manifest is the only checksum this reads. SHA256SUMS sits beside it and
# carries the same digests, so fetching both would prove nothing extra: they
# come from one origin over one TLS session. It stays published for people
# verifying a download by hand.
if [ -z "$sha256" ]; then
    die "${MANIFEST_URL} lists ${file} for ${TARGET} with no sha256; refusing to install unverified bytes."
fi

# Everything downloaded lands in WORK_DIR, and STAGED names the half-written
# copy inside the install dir. Both go on every exit path: the signal traps
# exit, and exiting runs the EXIT trap.
WORK_DIR=''
STAGED=''
# Reached only through the traps below, which shellcheck does not follow — it
# reads the body as both uncalled (SC2329) and unreachable (SC2317).
# shellcheck disable=SC2317,SC2329
cleanup() {
    # run_live hides the cursor and turns off line wrap while its spinner
    # runs; an interrupt must not leave the user's terminal that way.
    if [ "${TERM_MODES_CHANGED:-no}" = yes ]; then
        # Stopped first, or it can draw again after the lines are erased.
        kill "${run_spinner:-}" 2>/dev/null || true
        printf '\r\033[2K\n\033[2K\033[1A\r\033[?7h\033[?25h'
    fi
    if [ -n "$STAGED" ] && [ -e "$STAGED" ]; then
        rm -f "$STAGED"
    fi
    if [ -n "$WORK_DIR" ]; then
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM HUP

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/mapbox-cli.XXXXXX")" ||
    die "could not create a temporary directory."

# The manifest names the artifact relative to the channel directory. Keep only
# the basename for the local copy, so a manifest that ever carries a path
# prefix still writes somewhere that exists.
artifact_url="${BASE_URL}/${VERSION}/${file}"
tarball="${WORK_DIR}/${file##*/}"

case "$TARGET" in
    aarch64-apple-darwin) platform_name='macOS (Apple Silicon)' ;;
    x86_64-apple-darwin) platform_name='macOS (Intel)' ;;
    aarch64-*-linux-*) platform_name='Linux (arm64)' ;;
    *) platform_name='Linux (x86_64)' ;;
esac
echo ""
echo "${BOLD}Installing Mapbox CLI ${resolved_version:-$VERSION}${RESET} ${DIM}for ${platform_name}${RESET}"
echo ""

# A progress bar when someone is watching, and only then: in a log, curl's
# carriage-return redraws are noise. Once the download is done the bar has
# nothing left to say, so it is erased and the step line takes its place.
if [ -t 2 ]; then
    if [ -n "$AUTH" ]; then
        curl -fSL --progress-bar -A "$USER_AGENT" -u "$AUTH" -o "$tarball" "$artifact_url"
    else
        curl -fSL --progress-bar -A "$USER_AGENT" -o "$tarball" "$artifact_url"
    fi || die "could not download ${artifact_url}"
    printf '\033[1A\033[2K' >&2
else
    fetch -o "$tarball" "$artifact_url" ||
        die "could not download ${artifact_url}"
fi

# Before unpacking, not after: an artifact that fails here never gets the
# chance to write anything, anywhere.
actual_sha256="$(sha256_of "$tarball")"
if [ "$actual_sha256" != "$sha256" ]; then
    cat >&2 <<EOF
mapbox-cli: checksum mismatch for ${file} — nothing was installed.

  expected  ${sha256}
  actual    ${actual_sha256}

Either the download was corrupted or the artifact is not the one the manifest
describes. Try again; if it repeats, do not install it — report it at
${REPO}/issues
EOF
    exit 1
fi

size_bytes="$(wc -c <"$tarball" | tr -d ' ')"
size="$(awk -v b="$size_bytes" 'BEGIN {
    if (b >= 1048576) printf "%.1f MB", b / 1048576; else printf "%d KB", (b + 1023) / 1024
}')"
step_ok "Downloaded ${file##*/} ${DIM}(${size}, SHA-256 verified)${RESET}"

tar -xzf "$tarball" -C "$WORK_DIR" ||
    die "could not unpack ${file}"

[ -f "${WORK_DIR}/mapbox" ] ||
    die "${file} does not contain a mapbox binary at its root."

if [ ! -d "$INSTALL_DIR" ]; then
    mkdir -p "$INSTALL_DIR" ||
        die "could not create ${INSTALL_DIR}"
fi

if [ ! -w "$INSTALL_DIR" ]; then
    cat >&2 <<EOF
mapbox-cli: ${INSTALL_DIR} exists but is not writable.

Install somewhere you own instead:

    curl -fsSL ${BASE_URL}/install.sh | MAPBOX_INSTALL_DIR=\$HOME/.local/bin sh

This script never calls sudo on your behalf. If that directory really is where
you want the binary, re-run it with the privileges to write there.
EOF
    exit 1
fi

# What is already here, before anything is replaced. A `mapbox` from Homebrew
# or `cargo install` lives somewhere else entirely; that one is reported after
# the install rather than touched.
previous_version=''
if [ -x "${INSTALL_DIR}/mapbox" ]; then
    previous_version="$("${INSTALL_DIR}/mapbox" --version 2>/dev/null </dev/null || true)"
fi

# Write under a temp name in the *same* directory, then rename. A rename
# within one directory is atomic, so a concurrent or interrupted install can
# never leave behind a half-written binary that is still executable.
STAGED="${INSTALL_DIR}/.mapbox.install.$$"
cp "${WORK_DIR}/mapbox" "$STAGED" ||
    die "could not write to ${INSTALL_DIR}"
chmod 755 "$STAGED"
mv -f "$STAGED" "${INSTALL_DIR}/mapbox" ||
    die "could not install into ${INSTALL_DIR}"
STAGED=''


# Report what the binary says about itself rather than what the manifest
# claimed: an artifact built for another platform fails here and nowhere
# earlier.
installed_version="$("${INSTALL_DIR}/mapbox" --version 2>/dev/null </dev/null || true)"
[ -n "$installed_version" ] ||
    die "installed ${INSTALL_DIR}/mapbox, but it does not run here. The artifact may be built for a platform other than ${TARGET}."

if [ -n "$previous_version" ] && [ "$previous_version" != "$installed_version" ]; then
    step_ok "Installed ${installed_version} to $(tildify "${INSTALL_DIR}/mapbox") ${DIM}(replaced ${previous_version})${RESET}"
else
    step_ok "Installed ${installed_version} to $(tildify "${INSTALL_DIR}/mapbox")"
fi

# --- PATH ------------------------------------------------------------------
#
# Two separate things can be wrong, and both are worth saying: the install dir
# may not be on PATH at all, and even when it is, a `mapbox` from somewhere
# else may still come first.
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) install_dir_on_path=yes ;;
    *) install_dir_on_path=no ;;
esac

# $HOME stays literal in the line a profile gets, so a dotfile synced to a
# machine with another username still points at the right place.
case "$INSTALL_DIR" in
    "$HOME"/*) path_dir="\$HOME/${INSTALL_DIR#"$HOME"/}" ;;
    *) path_dir="$INSTALL_DIR" ;;
esac
path_line="export PATH=\"${path_dir}:\$PATH\""
case "${SHELL:-}" in
    */fish)
        profile="$HOME/.config/fish/config.fish"
        path_line="fish_add_path \"${path_dir}\""
        ;;
    */zsh) profile="${ZDOTDIR:-$HOME}/.zshrc" ;;
    */bash)
        # macOS Terminal opens login shells, which read .bash_profile and
        # never .bashrc — or rather the first of .bash_profile, .bash_login
        # and .profile that exists, and only that one. Creating a
        # .bash_profile next to someone's .profile would stop bash reading
        # their .profile at all, so an existing one wins.
        if [ "$(uname -s)" = Darwin ]; then
            profile="$HOME/.bash_profile"
            for candidate in .bash_profile .bash_login .profile; do
                if [ -f "$HOME/$candidate" ]; then
                    profile="$HOME/$candidate"
                    break
                fi
            done
        else
            profile="$HOME/.bashrc"
        fi
        ;;
    *) profile="$HOME/.profile" ;;
esac

# Whether a profile line other than a comment names <dir> as a whole path,
# not as a prefix of another (/x/bin in /x/bin2) or a word inside one. Catches
# a line the user wrote by hand, in either spelling of the directory.
profile_names_dir() { # profile_names_dir <profile> <dir>
    awk -v d="$2" -v before=":=\"' (	" -v after=":\"' )	" '
        /^[ \t]*#/ { next }
        {
            off = 0
            rest = $0
            while ((i = index(rest, d)) > 0) {
                at = off + i
                b = at > 1 ? substr($0, at - 1, 1) : ""
                a = substr($0, at + length(d), 1)
                if ((b == "" || index(before, b)) && (a == "" || index(after, a))) { found = 1; exit }
                off = at
                rest = substr($0, at + 1)
            }
        }
        END { exit !found }' "$1" 2>/dev/null
}

# yes when a profile now puts the install dir on PATH but this shell, which
# a child process cannot change, does not have it yet.
needs_new_shell=no
if [ "$install_dir_on_path" = no ]; then
    if [ "$MODIFY_PATH" = no ]; then
        step_warn "$(tildify "$INSTALL_DIR") is not on your PATH, and MAPBOX_NO_MODIFY_PATH is set. Add it:"
        echo ""
        echo "      echo '${path_line}' >> $(tildify "$profile")"
        echo ""
    # A second run must not append a second copy.
    elif [ -f "$profile" ] &&
        { grep -qF -- "$path_line" "$profile" 2>/dev/null ||
            profile_names_dir "$profile" "$INSTALL_DIR" ||
            profile_names_dir "$profile" "$path_dir"; }; then
        step_ok "PATH is already set up in $(tildify "$profile")"
        needs_new_shell=yes
    elif mkdir -p "$(dirname "$profile")" 2>/dev/null &&
        printf '\n# Added by the Mapbox CLI installer\n%s\n' "$path_line" >>"$profile" 2>/dev/null; then
        step_ok "Added $(tildify "$INSTALL_DIR") to PATH in $(tildify "$profile")"
        needs_new_shell=yes
    else
        step_warn "Could not write to $(tildify "$profile"). Add $(tildify "$INSTALL_DIR") to your PATH yourself:"
        echo ""
        echo "      ${path_line}"
        echo ""
    fi
fi

# `command -v` names the winner and stops, so a second mapbox further down
# PATH is invisible to it — and that is the copy which goes on being reported
# after this script has said it installed another one. A shell caches the path
# it resolved for a command name, nothing a child process does can clear its
# parent's cache, and this script is a child process: naming the other file
# and the command that drops the cache is the most it can do. One hit is
# enough, because the advice does not get better with a list.
other_mapbox=''
install_dir_entry="${INSTALL_DIR%/}"
saved_ifs="$IFS"
IFS=':'
# Splitting on the colons is the point here, and a PATH entry holding a glob
# character would be pathological.
# shellcheck disable=SC2086
for dir in $PATH; do
    case "$dir" in
        # An empty entry means the current directory, which is where a PATH
        # search looks too.
        '') dir='.' ;;
        ?*/) dir="${dir%/}" ;;
    esac
    [ "$dir" != "$install_dir_entry" ] || continue
    if [ -f "${dir}/mapbox" ] && [ -x "${dir}/mapbox" ]; then
        other_mapbox="${dir}/mapbox"
        break
    fi
done
IFS="$saved_ifs"

# A profile line written just now prepends the install dir, so a new shell
# runs the copy just installed whatever this one resolves; the new-terminal
# hint below already covers this shell.
resolved="$(command -v mapbox 2>/dev/null || true)"
if [ -n "$resolved" ] && [ "$resolved" != "${INSTALL_DIR}/mapbox" ] && [ "$needs_new_shell" = no ]; then
    step_warn "mapbox on your PATH still resolves to ${resolved}"
    cat <<EOF
    That one came from somewhere else (Homebrew, cargo install, an earlier
    PATH entry) and this script did not touch it. To use the copy just
    installed, remove it or put $(tildify "$INSTALL_DIR") ahead of it on PATH, then
    run hash -r (rehash, in zsh).
EOF
elif [ -n "$other_mapbox" ] && [ "$install_dir_on_path" = yes ]; then
    step_warn "There is another mapbox at ${other_mapbox}"
    cat <<EOF
    $(tildify "$INSTALL_DIR") comes first on your PATH, so a new terminal runs the copy
    just installed. A shell that already ran mapbox has the old path cached:
    run hash -r (rehash, in zsh), and then mapbox --version
    prints ${installed_version}. This script did not touch the other copy.
EOF
fi

# Under `curl ... | sh` stdin is the script, so a prompt has to come from the
# terminal directly. /dev/tty exists as a device inside a container with no
# terminal attached and fails only on open, so open it to find out. Shared by
# both optional steps below, Tilesets and the coding agent skill — neither
# owns it.
have_tty() {
    [ -c /dev/tty ] || return 1
    (exec 3</dev/tty) 2>/dev/null
}

# Prompt on the terminal, answer on stdout, so a caller can capture one
# without swallowing the other.
ask() {
    printf '%s' "$1" >/dev/tty
    ask_answer=''
    IFS= read -r ask_answer </dev/tty || return 1
    printf '%s' "$ask_answer"
}

is_yes() {
    case "$1" in
        y | Y | yes | Yes | YES | true | TRUE | 1) return 0 ;;
        *) return 1 ;;
    esac
}

is_no() {
    case "$1" in
        n | N | no | No | NO | false | FALSE | 0) return 0 ;;
        *) return 1 ;;
    esac
}

# --- Coding agent skill ------------------------------------------------------
#
# Most machines running this script have no coding agent installed at all,
# and generate-skills answers "nowhere to write" as `no_agent_detected`
# rather than writing anything when that is the case (src/skill_dest.rs) —
# checked first, silently, so nothing is offered when there is nothing to
# offer. --global, not the project-scoped default: this script runs in
# whatever directory the shell happened to be in, which has no relation to
# a project.
#
# Past that point this asks, the same way the Tilesets CLI below does:
# writing into a directory this CLI does not own and downloading a whole
# separate library is a bigger ask than a one-line telemetry notice, and an
# install nobody consented to is not a feature. An interactive terminal
# asks and installs only on yes; no terminal says nothing was installed and
# how to do it by hand instead.
agent_setup_allowed() {
    # Same boolean spellings as MAPBOX_CLI_NO_TELEMETRY above: unset, empty
    # or one of clap's false spellings is allowed; anything else opts out.
    # ASCII-only on purpose, the same reason telemetry_allowed is above.
    # shellcheck disable=SC2018,SC2019
    case "$(printf '%s' "${MAPBOX_CLI_NO_AGENT_SETUP-}" |
        tr 'A-Z' 'a-z' |
        sed 's/^[[:space:]]*//;s/[[:space:]]*$//')" in
        '' | 0 | f | false | n | no | off) return 0 ;;
        *) return 1 ;;
    esac
}

# The "source" field of each destination in a generate-skills/agent-skills
# --global JSON report is "<Agent label>, all projects" — this is the part
# worth putting in front of a person before asking them anything, not the
# full file listing the real report carries.
agent_names_from() { # agent_names_from <json> : comma-joined agent labels
    printf '%s' "$1" | grep -o '"source":"[^"]*"' |
        sed 's/"source":"//;s/, all projects"$//' |
        paste -sd, - | sed 's/,/, /g'
}

if agent_setup_allowed; then
    if agent_check=$(log_capture "${INSTALL_DIR}/mapbox" generate-skills --global --dry-run -o json); then
        agent_names="$(agent_names_from "$agent_check")"
        step_ok "Coding agents found: ${agent_names}."
        do_agent_setup=no
        if have_tty; then
            ask_title "Set up the mapbox CLI skill and the Mapbox Agent Skills library for them?"
            ask_note "This runs $(ask_cmd 'mapbox generate-skills --global') and $(ask_cmd 'mapbox agent-skills install --global'),"
            ask_note "now or any time later."
            answer="$(ask "    Set them up now? ${DIM}[y/N]${RESET} ")" || answer=''
            is_yes "$answer" && do_agent_setup=yes
            STEP_INDENT='    '
        fi
        if [ "$do_agent_setup" = yes ]; then
            # Offline and safe to re-run: it replaces its own generated
            # directory wholesale and refuses only if something else
            # already lives there, which --global keeps out of this
            # script's way.
            if log_run "${INSTALL_DIR}/mapbox" generate-skills --global; then
                step_ok "Wrote the mapbox CLI skill for: ${agent_names}."
            fi

            # stderr is kept with stdout here: already_installed arrives there.
            log "\$ ${INSTALL_DIR}/mapbox agent-skills install --global -o json"
            if agent_out=$("${INSTALL_DIR}/mapbox" agent-skills install --global -o json 2>&1 </dev/null); then agent_status=0; else agent_status=$?; fi
            [ -z "$agent_out" ] || log "$agent_out"
            log "(exit ${agent_status})"
            if [ "$agent_status" = 0 ]; then
                step_ok "Installed the Mapbox Agent Skills library for: ${agent_names}."
            else
                case "$agent_out" in
                    # A reinstall/upgrade: the skill directory is already
                    # there from a previous run of this same script.
                    # `update` is the one safe to repeat, but only once it
                    # is checked first — some of what changed since the
                    # last run might be a local edit, not just an upstream
                    # refresh, and that is never overwritten without being
                    # named.
                    *'"code":"already_installed"'*)
                        if update_check=$(log_capture "${INSTALL_DIR}/mapbox" agent-skills update --global --dry-run -o json); then
                            case "$update_check" in
                                *'"updated":[]'*)
                                    log_run "${INSTALL_DIR}/mapbox" agent-skills update --global || true
                                    step_ok "Updated the Mapbox Agent Skills library for: ${agent_names}."
                                    ;;
                                *)
                                    step_warn "Some Mapbox Agent Skills have local changes and were left alone."
                                    step_detail "Run 'mapbox agent-skills update --global' to review and replace them."
                                    ;;
                            esac
                        fi
                        ;;
                esac
            fi
        else
            step_skip "Not set up. Run these any time:"
            step_detail "${DIM}mapbox generate-skills --global${RESET}"
            step_detail "${DIM}mapbox agent-skills install --global${RESET}"
        fi
        STEP_INDENT='  '
    fi
fi

# --- Mapbox MCP servers ------------------------------------------------------
#
# A separate question from the skill above: this registers a hosted server in
# the coding agent's own config, next to servers the user added, rather than
# writing a directory this CLI owns. `mapbox mcp install` never replaces an
# entry that is already there (src/mcp.rs), so on a reinstall it is only
# offered when something is left to register. Same opt-out and same
# no-terminal rule as the skill.

# "status|server|client" per row of `mapbox mcp install -o json`, with the
# labels src/mcp.rs prints in place of its ids.
# Each row is a flat object, read field by field: the keys come sorted, so a
# row that carries "error" has it between "client" and "server".
mcp_field() { # mcp_field <key> <object>
    printf '%s' "$2" | sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"
}
mcp_rows() { # mcp_rows <json>
    # "client" sorts first, so every row starts with it; splitting there
    # keeps a row whole even when its "error" text has braces of its own.
    printf '%s' "$1" | awk '{ gsub(/[{]"client":"/, "\n{\"client\":\""); print }' | while IFS= read -r mcp_row; do
        mcp_s="$(mcp_field status "$mcp_row")"
        mcp_v="$(mcp_field server "$mcp_row")"
        mcp_c="$(mcp_field client "$mcp_row")"
        if [ -n "$mcp_s" ] && [ -n "$mcp_v" ] && [ -n "$mcp_c" ]; then
            printf '%s|%s|%s\n' "$mcp_s" "$mcp_v" "$mcp_c"
        fi
    done |
        sed 's/|mapbox|/|Mapbox MCP|/;s/|mapbox-devkit|/|Mapbox DevKit MCP|/' |
        sed 's/|claude-code$/|Claude Code/;s/|codex$/|Codex/;s/|vscode$/|VS Code/;s/|cursor$/|Cursor/'
}

# "status|server|client, client, ..." — one line per server and outcome, in
# the order they first appear, so four results read as two lines.
mcp_grouped() { # mcp_grouped <json>
    mcp_rows "$1" | awk -F'|' '{
        k = $1 "|" $2
        if (!(k in who)) { order[++n] = k; who[k] = $3 } else { who[k] = who[k] ", " $3 }
    } END { for (i = 1; i <= n; i++) print order[i] "|" who[order[i]] }'
}

# The clients a dry run would register anything with, comma-joined.
mcp_clients_from() { # mcp_clients_from <json>
    mcp_rows "$1" | awk -F'|' '$1 == "would_install" && !seen[$3]++ { print $3 }' |
        paste -sd, - | sed 's/,/, /g'
}

if agent_setup_allowed; then
    # Fails with mcp_client_not_found when no supported client is installed,
    # which is most machines: nothing to offer, so nothing is said.
    if mcp_check=$(log_capture "${INSTALL_DIR}/mapbox" mcp install --global --dry-run -o json); then
        mcp_clients="$(mcp_clients_from "$mcp_check")"
        if [ -n "$mcp_clients" ]; then
            do_mcp_setup=no
            if have_tty; then
                ask_title "Add the Mapbox MCP servers to ${mcp_clients}?"
                ask_note "This runs $(ask_cmd 'mapbox mcp install --global'), now or any time later."
                answer="$(ask "    Add them now? ${DIM}[y/N]${RESET} ")" || answer=''
                is_yes "$answer" && do_mcp_setup=yes
                STEP_INDENT='    '
            fi
            if [ "$do_mcp_setup" = yes ]; then
                # Each client's own CLI does the registering, and Codex's can
                # stop to print a sign-in URL, so its output is shown live.
                if run_live "Adding the Mapbox MCP servers" "${WORK_DIR}/mcp-install.log" "${WORK_DIR}/mcp-install.json" \
                    "${INSTALL_DIR}/mapbox" mcp install --global -o json; then
                    mcp_ok=yes
                else
                    mcp_ok=no
                fi
                mcp_grouped "$(cat "${WORK_DIR}/mcp-install.json" 2>/dev/null)" >"${WORK_DIR}/mcp-rows"
                # One line per server and outcome, so a server added just now
                # reads differently from one that was already there.
                mcp_trouble=no
                while IFS='|' read -r status what who; do
                    case "$status" in
                        installed | already_installed) ;;
                        *) mcp_trouble=yes ;;
                    esac
                    case "$status" in
                        installed) step_ok "Added ${what} to ${who}" ;;
                        already_installed) step_skip "${what} was already in ${who}" ;;
                        installed_login_incomplete) step_warn "Added ${what} to ${who}, but sign-in did not finish" ;;
                        client_not_found) step_warn "Skipped ${what} for ${who}: CLI not on PATH" ;;
                        config_unreadable) step_warn "Skipped ${what} for ${who}: config could not be read" ;;
                        *) step_warn "Could not add ${what} to ${who}" ;;
                    esac
                done <"${WORK_DIR}/mcp-rows"
                if [ "$mcp_ok" = no ]; then
                    # A failure no row above accounts for still gets a line of
                    # its own, rather than a list of successes.
                    [ "$mcp_trouble" = yes ] || step_warn "Not every Mapbox MCP server could be added."
                    step_detail "Run 'mapbox mcp install --global' to see why and try again."
                fi
            else
                step_skip "Mapbox MCP servers not added to ${mcp_clients}. Run this any time:"
                step_detail "${DIM}mapbox mcp install --global${RESET}"
            fi
            STEP_INDENT='  '
        fi
    fi
fi

# --- The Tilesets CLI ------------------------------------------------------
#
# `mapbox tilesets-cli` execs a separately installed `tilesets`. When an
# install was tried and failed, what is printed says the same thing as
# `launch_failed` in src/tilesets_cli.rs, in another language; keep the two in
# step. A plain "no" gets one line instead, since `mapbox tilesets-cli` prints
# the full version itself the first time it is run without one.

tilesets_instructions() {
    cat <<EOF

    To set it up later — the same instructions mapbox tilesets-cli prints when
    it cannot find it:

        pipx install mapbox-tilesets            # recommended, keeps it isolated
        python3 -m pip install --user mapbox-tilesets

    If it lands somewhere not on your PATH, point the CLI straight at it:

        export MAPBOX_TILESETS_CLI=/path/to/tilesets

    Docs: https://github.com/mapbox/tilesets-cli
EOF
}

# `src/tilesets_cli.rs`'s `launch_failed` names this same requirement when
# `tilesets` can't be found; change one and change the other.
python3_at_least_310() {
    command -v python3 >/dev/null 2>&1 || return 1
    python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' \
        </dev/null >/dev/null 2>&1
}

# A distro-packaged interpreter marks itself externally managed (PEP 668) and
# refuses `pip install --user` outright — Debian 12 and Ubuntu 23.04 onward,
# which is most Linux installs. Running pip there is a guess whose answer is
# already known, so ask the marker file rather than asking pip.
python3_externally_managed() {
    python3 -c 'import os, sysconfig, sys; sys.exit(0 if os.path.exists(os.path.join(sysconfig.get_path("stdlib"), "EXTERNALLY-MANAGED")) else 1)' \
        </dev/null >/dev/null 2>&1
}

# pipx and pip talk at length (emoji included), and once they are done say
# nothing the step line after them does not, so run_live shows them only
# while they run.
TILESETS_LOG="${WORK_DIR}/tilesets-install.log"

# 0 installed, 1 an installer ran and failed, 2 no interpreter to install with,
# 3 an interpreter that will not be installed into.
# Children get </dev/null because stdin is still the rest of this script.
install_tilesets() {
    if command -v pipx >/dev/null 2>&1; then
        run_live "Installing the Tilesets CLI with pipx ${DIM}(this can take a minute)${RESET}" "$TILESETS_LOG" '' \
            pipx install mapbox-tilesets || return 1
        return 0
    fi
    if python3_at_least_310; then
        if python3_externally_managed; then
            return 3
        fi
        run_live "Installing the Tilesets CLI with pip --user ${DIM}(pipx is not installed)${RESET}" "$TILESETS_LOG" '' \
            python3 -m pip install --user mapbox-tilesets || return 1
        return 0
    fi
    return 2
}

tilesets_step() {
    # Honor MAPBOX_TILESETS_CLI, the override the CLI itself respects: someone
    # who has pointed it at a particular executable has already made this
    # decision, whether or not that executable is currently there.
    if [ -n "${MAPBOX_TILESETS_CLI:-}" ]; then
        if [ -x "${MAPBOX_TILESETS_CLI}" ]; then
            step_ok "Found Tilesets CLI: ${MAPBOX_TILESETS_CLI} (MAPBOX_TILESETS_CLI)"
        else
            step_warn "MAPBOX_TILESETS_CLI points at ${MAPBOX_TILESETS_CLI}, where there is no"
            step_detail "executable. mapbox tilesets-cli will fail until that is corrected or unset."
        fi
        return
    fi

    if command -v tilesets >/dev/null 2>&1; then
        step_ok "Found Tilesets CLI: $(tilesets --version 2>/dev/null </dev/null || command -v tilesets)"
        return
    fi

    answer=''
    if [ -n "$INSTALL_TILESETS" ]; then
        if is_yes "$INSTALL_TILESETS"; then
            answer=yes
        elif is_no "$INSTALL_TILESETS"; then
            answer=no
        else
            step_warn "MAPBOX_INSTALL_TILESETS=${INSTALL_TILESETS} is neither yes nor no; not installing."
            answer=no
        fi
    elif have_tty; then
        # mapbox is already on disk and has answered --version by now, and the
        # step lines above say so: this is an extra, offered as one.
        answer="$(ask "  ${ACCENT}${BOLD}${MARK_ASK}${RESET} ${BOLD}Install the Tilesets CLI too?${RESET} ${DIM}Only the${RESET} ${ACCENT}mapbox tilesets-cli${RESET} ${DIM}command needs it. [y/N]${RESET} ")" ||
            answer=''
        if is_yes "$answer"; then
            answer=yes
        else
            answer=no
        fi
        STEP_INDENT='    '
    else
        # No terminal: a provisioner, a Docker build, CI. Do not block, and do
        # not assume yes — mutating someone's Python environment unasked is
        # exactly what the prompt exists to avoid.
        answer=no
    fi

    if [ "$answer" != yes ]; then
        step_skip "Tilesets CLI is not installed. Only the ${ACCENT}mapbox tilesets-cli${RESET} command needs it."
        step_detail "${DIM}Install it later with: pipx install mapbox-tilesets${RESET}"
        return
    fi

    # The `if` keeps a failed install from tripping `set -e`; its status is
    # read in the else branch.
    if install_tilesets; then
        if command -v tilesets >/dev/null 2>&1; then
            step_ok "Installed Tilesets CLI: $(tilesets --version 2>/dev/null </dev/null || echo tilesets)"
        else
            user_bin="$(python3 -m site --user-base 2>/dev/null </dev/null || true)"
            step_warn "Installed mapbox-tilesets, but tilesets is not on your PATH."
            if [ -n "$user_bin" ]; then
                step_detail "Look in ${user_bin}/bin, then add that to PATH or set MAPBOX_TILESETS_CLI."
            else
                step_detail "Add its directory to PATH, or set MAPBOX_TILESETS_CLI to the executable."
            fi
        fi
    else
        rc=$?
        if [ "$rc" = 2 ]; then
            step_warn "Cannot install it here: no pipx, and no Python 3.10+ interpreter to use instead."
        elif [ "$rc" = 3 ]; then
            step_warn "Cannot install it here: this python3 is managed by your OS (PEP 668)."
            step_detail "It refuses a pip --user install, and there is no pipx to use instead."
            step_detail "Install pipx first — apt install pipx, or brew install pipx — and the"
            step_detail "first command below is the one that works."
        else
            step_warn "The Tilesets CLI install did not succeed. The installer said:"
            echo ""
            tail -n 20 "$TILESETS_LOG" | sed "s/^/${STEP_INDENT}  /"
        fi
        # Its text is indented for a top-level step; shift it to match.
        tilesets_instructions | sed "s/^\\(.\\)/${STEP_INDENT#  }\\1/"
    fi
}

tilesets_step
STEP_INDENT='  '

# The log closes the step list: it is the record of those steps, and where
# to look again at anything above that went by too fast or went wrong.
if [ -n "$INSTALL_LOG" ]; then
    echo "  ${DIM}Install log: $(tildify "$INSTALL_LOG")${RESET}"
fi

# --- Summary ---------------------------------------------------------------
#
# The last thing on screen, because it is what the reader acts on: whatever
# happened with the optional extra above, mapbox itself is installed.

echo ""
echo "${BOLD}${GREEN}${installed_version} is ready.${RESET}"

if [ "$needs_new_shell" = yes ]; then
    echo ""
    # The profile line reaches shells started from now on; this one read
    # its profile before the line was there.
    echo "This terminal started before that change. Open a new one, or run:"
    echo ""
    echo "  ${ACCENT}${path_line}${RESET}"
fi

echo ""
echo "${BOLD}Get started${RESET}"
printf '  %s%-20s%s %s\n' "$ACCENT" 'mapbox auth login' "$RESET" 'Sign in to your Mapbox account'
printf '  %s%-20s%s %s\n' "$ACCENT" 'mapbox --help' "$RESET" 'See every command'
echo ""
echo "Docs: https://cli.mapbox.com"

# Said once, here, rather than on every future command: someone piping this
# into `sh` is not going to read the man page before their first run.
# Skipped when telemetry is already off, since there's nothing to opt out of.
if telemetry_allowed; then
    echo ""
    echo "Mapbox CLI collects telemetry by default. To disable it, set"
    echo "MAPBOX_CLI_NO_TELEMETRY=1. Learn more: https://github.com/mapbox/mapbox-cli#privacy"
fi
echo ""

exit 0
