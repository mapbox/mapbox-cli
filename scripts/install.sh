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
INSTALL_DIR="${MAPBOX_INSTALL_DIR:-$HOME/.local/bin}"

# Where a reader is sent to build from source or report a bad artifact. One
# variable rather than three literals, the way install.ps1 keeps its $Repo:
# this script is served on its own, so a repository rename has to be a single
# edit here and a single edit there.
REPO='https://github.com/mapbox/cli'

# yes or no to answer the Tilesets CLI prompt ahead of time. Unset means ask
# when there is a terminal, and no when there isn't.
INSTALL_TILESETS="${MAPBOX_INSTALL_TILESETS:-}"

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
INSTALL_SOURCE="${MAPBOX_CLI_INSTALL_SOURCE:-}"

# The switch `src/telemetry.rs` honours for the CLI's own User-Agent, read here
# the same way, because someone who put it in a Dockerfile and then pipes this
# script into sh in the same file has already said which way they want it. The
# product token above is what survives it — the equivalent of
# `mapbox-cli/<version>` going out either way — and everything appended below
# is what it drops.
#
# **Two names, and only the binary dropped the old one.**
# `MAPBOX_CLI_NO_TELEMETRY` is the documented switch; `DISABLE_TELEMETRY` is
# what it was called before, and this script still honours it. The rename was
# announced as breaking for the binary, so a `DISABLE_TELEMETRY=1` there
# genuinely stopped working and the changelog says so. Nothing announced it for
# the installers — this file is fetched and run in one line, so a reader has no
# release notes in front of them — and breaking an opt-out is the one change
# that must not happen quietly. So both work here, the new name wins when both
# are set, and the old one keeps working for the Dockerfile the comment above
# describes. See mapbox/mapbox-cli-private#140.
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

die() {
    echo "mapbox-cli: $*" >&2
    exit 1
}

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

echo "Downloading ${artifact_url}"
fetch -o "$tarball" "$artifact_url" ||
    die "could not download ${artifact_url}"

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

echo ""
echo "Installed ${installed_version}"
echo "  path     ${INSTALL_DIR}/mapbox"
echo "  channel  ${VERSION} (${resolved_version})"
if [ -n "$previous_version" ] && [ "$previous_version" != "$installed_version" ]; then
    echo "  replaced ${previous_version}"
fi

# Two separate things can be wrong, and both are worth saying: the install dir
# may not be on PATH at all, and even when it is, a `mapbox` from somewhere
# else may still come first.
case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) install_dir_on_path=yes ;;
    *) install_dir_on_path=no ;;
esac

if [ "$install_dir_on_path" = no ]; then
    path_line="export PATH=\"${INSTALL_DIR}:\$PATH\""
    # The ~ stays literal on purpose: this is a line for the user to read and
    # type, and no path here is ever opened.
    # shellcheck disable=SC2088
    case "${SHELL:-}" in
        */fish)
            path_file="~/.config/fish/config.fish"
            path_line="fish_add_path ${INSTALL_DIR}"
            ;;
        */zsh) path_file="~/.zshrc" ;;
        */bash)
            # macOS Terminal opens login shells, which read .bash_profile and
            # never .bashrc.
            if [ "$(uname -s)" = Darwin ]; then
                path_file="~/.bash_profile"
            else
                path_file="~/.bashrc"
            fi
            ;;
        *) path_file="~/.profile" ;;
    esac
    cat <<EOF

${INSTALL_DIR} is not on your PATH. Add it:

    echo '${path_line}' >> ${path_file}

Then restart your shell, or run that line now to use mapbox in this one.
EOF
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

resolved="$(command -v mapbox 2>/dev/null || true)"
if [ -n "$resolved" ] && [ "$resolved" != "${INSTALL_DIR}/mapbox" ]; then
    cat <<EOF

Note: mapbox on your PATH still resolves to ${resolved}, which came from
somewhere else — Homebrew, cargo install, an install directory earlier on
PATH. This script did not touch it. To use the copy just installed, remove
that one or put ${INSTALL_DIR} ahead of it on PATH, then run hash -r (rehash,
in zsh) in any shell that has already run mapbox — it has the old path cached.
EOF
elif [ -n "$other_mapbox" ]; then
    cat <<EOF

Note: there is another mapbox at ${other_mapbox}. ${INSTALL_DIR} comes first
on your PATH, so a new shell runs the copy just installed — but a shell that
has already run mapbox has the old path cached, and goes on reporting the old
version. Drop that cache, or open a new terminal:

    hash -r        # rehash, in zsh

Then mapbox --version prints ${installed_version}. Removing ${other_mapbox}
stops this happening again; this script did not touch it.
EOF
fi

# --- The Tilesets CLI ------------------------------------------------------
#
# `mapbox tilesets-cli` execs a separately installed `tilesets`. Everything
# below says the same thing as `launch_failed` in src/tilesets_cli.rs, in
# another language; keep the two in step.

tilesets_instructions() {
    cat <<EOF

To set it up later — the same instructions mapbox tilesets-cli prints when it
cannot find it:

    pipx install mapbox-tilesets            # recommended, keeps it isolated
    python3 -m pip install --user mapbox-tilesets

If it lands somewhere not on your PATH, point the CLI straight at it:

    export MAPBOX_TILESETS_CLI=/path/to/tilesets

Docs: https://github.com/mapbox/tilesets-cli
EOF
}

# Under `curl ... | sh` stdin is the script, so a prompt has to come from the
# terminal directly. /dev/tty exists as a device inside a container with no
# terminal attached and fails only on open, so open it to find out.
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

# 0 installed, 1 an installer ran and failed, 2 no interpreter to install with,
# 3 an interpreter that will not be installed into.
# Children get </dev/null because stdin is still the rest of this script.
install_tilesets() {
    if command -v pipx >/dev/null 2>&1; then
        echo "Running: pipx install mapbox-tilesets"
        pipx install mapbox-tilesets </dev/null || return 1
        return 0
    fi
    if python3_at_least_310; then
        if python3_externally_managed; then
            return 3
        fi
        echo "pipx is not installed; using pip instead."
        echo "Running: python3 -m pip install --user mapbox-tilesets"
        python3 -m pip install --user mapbox-tilesets </dev/null || return 1
        return 0
    fi
    return 2
}

# Honour MAPBOX_TILESETS_CLI, the override the CLI itself respects: someone
# who has pointed it at a particular executable has already made this
# decision, whether or not that executable is currently there.
if [ -n "${MAPBOX_TILESETS_CLI:-}" ]; then
    echo ""
    if [ -x "${MAPBOX_TILESETS_CLI}" ]; then
        echo "Tilesets CLI: ${MAPBOX_TILESETS_CLI} (MAPBOX_TILESETS_CLI)"
    else
        echo "Note: MAPBOX_TILESETS_CLI points at ${MAPBOX_TILESETS_CLI}, where there is no"
        echo "executable. mapbox tilesets-cli will fail until that is corrected or unset."
    fi
    exit 0
fi

if command -v tilesets >/dev/null 2>&1; then
    echo ""
    echo "Tilesets CLI: $(tilesets --version 2>/dev/null </dev/null || command -v tilesets)"
    exit 0
fi

# Leading with "not installed" read as though the install had gone wrong. It
# had not: by this point mapbox is on disk and has already answered
# --version. Say that first, then offer the extra as an extra.
cat <<EOF

mapbox is installed and ready to use — the rest of this is optional.

The Mapbox Tilesets CLI is not installed here. mapbox tilesets-cli ... forwards
to it, and tileset commands are the only part of this CLI that need it; every
other command already works. It ships separately as the Python package
mapbox-tilesets (Python 3.10+).
EOF

answer=''
if [ -n "$INSTALL_TILESETS" ]; then
    if is_yes "$INSTALL_TILESETS"; then
        answer=yes
    elif is_no "$INSTALL_TILESETS"; then
        answer=no
    else
        echo ""
        echo "Note: MAPBOX_INSTALL_TILESETS=${INSTALL_TILESETS} is neither yes nor no; not installing."
        answer=no
    fi
elif have_tty; then
    echo ""
    answer="$(ask 'Install it as well? [y/N] ')" || answer=''
    if is_yes "$answer"; then
        answer=yes
    else
        answer=no
    fi
else
    # No terminal: a provisioner, a Docker build, CI. Do not block, and do not
    # assume yes — mutating someone's Python environment unasked is exactly
    # what the prompt exists to avoid.
    answer=no
fi

if [ "$answer" != yes ]; then
    tilesets_instructions
    # A declined optional extra is not an install failure: mapbox is installed
    # and working.
    exit 0
fi

echo ""
if install_tilesets; then
    if command -v tilesets >/dev/null 2>&1; then
        echo ""
        echo "Tilesets CLI: $(tilesets --version 2>/dev/null </dev/null || echo installed)"
    else
        user_bin="$(python3 -m site --user-base 2>/dev/null </dev/null || true)"
        echo ""
        echo "Installed mapbox-tilesets, but tilesets is not on your PATH."
        if [ -n "$user_bin" ]; then
            echo "Look in ${user_bin}/bin, then add that to PATH or set MAPBOX_TILESETS_CLI."
        else
            echo "Add its directory to PATH, or set MAPBOX_TILESETS_CLI to the executable."
        fi
    fi
else
    rc=$?
    echo ""
    if [ "$rc" = 2 ]; then
        echo "Cannot install it here: no pipx, and no Python 3.10+ interpreter to use instead."
    elif [ "$rc" = 3 ]; then
        echo "Cannot install it here: this python3 is managed by your OS (PEP 668), so it"
        echo "refuses a pip --user install, and there is no pipx to use instead. Install"
        echo "pipx first — apt install pipx, or brew install pipx — and the first command"
        echo "below is the one that works."
    else
        echo "That install did not succeed."
    fi
    tilesets_instructions
fi

# Whatever happened above, the mapbox install succeeded.
exit 0
