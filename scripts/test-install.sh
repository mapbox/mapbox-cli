#!/bin/sh
# Exercises scripts/install.sh end to end without touching the network or
# anything outside a scratch directory: the channel is a directory served over
# file://, the "binary" is a shell script that prints a version, and pipx,
# python3, tilesets and uname are stood in for per case.
#
# Run it by hand with `sh scripts/test-install.sh`; ci.yml runs it on macOS and
# Linux both, because the portability it guards — shasum vs sha256sum, BSD vs
# GNU tar and script(1) — only shows up on one of them.
#
# python3 does two things here that a POSIX shell cannot do portably: run a
# child in a new session, so /dev/tty cannot be opened and the run looks like
# CI, a Docker build or a provisioner; and enforce a timeout, so a prompt that
# blocks fails the run instead of hanging it. install.sh itself still depends
# on nothing but curl, tar and a checksum tool.
set -eu

LC_ALL=C
export LC_ALL

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
INSTALLER="${REPO_DIR}/scripts/install.sh"
[ -f "$INSTALLER" ] || {
    echo "cannot find scripts/install.sh next to $0" >&2
    exit 1
}

# The other installer, read as text rather than run: this suite is POSIX sh on
# macOS and Linux, and the case that compares the two markers needs only to
# read the file. test-install.ps1 is what runs it.
PS_INSTALLER="${REPO_DIR}/scripts/install.ps1"

# The marker itself, taken from install.sh rather than written down again — the
# cases below assert what the installer actually sends, so there is no second
# copy of the string here to fall out of step with it. The pattern pins the
# product token; the version is whatever the installer says.
INSTALLER_UA="$(sed -n "s/^USER_AGENT='\(mapbox-cli-install\/[0-9][0-9]*\)'\$/\1/p" "$INSTALLER")"
[ -n "$INSTALLER_UA" ] || {
    echo "cannot find the mapbox-cli-install/<n> User-Agent in ${INSTALLER}" >&2
    exit 1
}

ROOT="$(mktemp -d "${TMPDIR:-/tmp}/mapbox-cli-tests.XXXXXX")"
trap 'rm -rf "$ROOT"' EXIT
OUT="${ROOT}/output"
SHIMS="${ROOT}/shims"

FAILURES=0

case "$(uname -s)/$(uname -m)" in
    Darwin/arm64) TARGET=aarch64-apple-darwin ;;
    Darwin/x86_64) TARGET=x86_64-apple-darwin ;;
    Linux/aarch64 | Linux/arm64) TARGET=aarch64-unknown-linux-musl ;;
    Linux/x86_64) TARGET=x86_64-unknown-linux-musl ;;
    *)
        echo "no target mapping for $(uname -s) $(uname -m); nothing to test" >&2
        exit 0
        ;;
esac

# The cases run with a PATH of their own rather than the caller's: a real
# `tilesets` or `pipx` in ~/.local/bin or /opt/homebrew/bin would otherwise
# decide the answer to half of them. Only the standard directories are kept,
# plus whatever holds a tool install.sh needs that is not in them.
SAFE_PATH='/usr/bin:/bin:/usr/sbin:/sbin'
for tool in curl tar sed tr cut mktemp uname python3; do
    if PATH="$SAFE_PATH" command -v "$tool" >/dev/null 2>&1; then
        continue
    fi
    tool_path="$(command -v "$tool" 2>/dev/null || true)"
    [ -n "$tool_path" ] || {
        echo "cannot run these tests without ${tool}" >&2
        exit 1
    }
    SAFE_PATH="${SAFE_PATH}:$(dirname "$tool_path")"
done
if ! PATH="$SAFE_PATH" command -v sha256sum >/dev/null 2>&1 &&
    ! PATH="$SAFE_PATH" command -v shasum >/dev/null 2>&1; then
    echo 'cannot run these tests without sha256sum or shasum' >&2
    exit 1
fi
# Resolved once, by absolute path, and out of SAFE_PATH rather than the
# caller's: cases put a fake python3 on PATH, and a pyenv shim would prepend
# its own version directory to PATH for everything it starts — which is how a
# real `tilesets` gets into a child that is supposed to have none.
PYTHON3="$(PATH="$SAFE_PATH" command -v python3 2>/dev/null || command -v python3 2>/dev/null || true)"
[ -n "$PYTHON3" ] || {
    echo 'these tests need python3 (install.sh does not)' >&2
    exit 1
}

for leaked in tilesets pipx; do
    if PATH="$SAFE_PATH" command -v "$leaked" >/dev/null 2>&1; then
        echo "warning: a real ${leaked} is on ${SAFE_PATH}; the Tilesets CLI" >&2
        echo "         cases assume neither pipx nor tilesets is installed" >&2
    fi
done

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# --- assertions ------------------------------------------------------------

start() { printf '\n%s\n' "$1"; }

pass() { printf '  ok    %s\n' "$1"; }

fail() {
    printf '  FAIL  %s\n' "$1"
    sed 's/^/      | /' "$OUT"
    FAILURES=$((FAILURES + 1))
}

expect_status() { # want got label
    if [ "$1" = "$2" ]; then
        pass "$3 (exit $2)"
    else
        fail "$3: wanted exit $1, got $2"
    fi
}

expect_out() { # needle label
    if grep -qF -- "$1" "$OUT"; then
        pass "$2"
    else
        fail "$2: no '$1' in output"
    fi
}

expect_in_file() { # file needle label
    if grep -qF -- "$2" "$1"; then
        pass "$3"
    else
        fail "$3: no '$2' in $1"
    fi
}

expect_not_in_file() { # file needle label
    if grep -qF -- "$2" "$1"; then
        fail "$3: '$2' is in $1"
    else
        pass "$3"
    fi
}

expect_no_out() { # needle label
    if grep -qF -- "$1" "$OUT"; then
        fail "$2: unexpected '$1' in output"
    else
        pass "$2"
    fi
}

expect_file() { # path label
    if [ -f "$1" ]; then
        pass "$2"
    else
        fail "$2: $1 does not exist"
    fi
}

expect_no_file() { # path label
    if [ -e "$1" ]; then
        fail "$2: $1 exists"
    else
        pass "$2"
    fi
}

expect_order() { # first second label
    line_a="$(grep -nF -- "$1" "$OUT" | head -1 | cut -d: -f1)"
    line_b="$(grep -nF -- "$2" "$OUT" | head -1 | cut -d: -f1)"
    if [ -n "$line_a" ] && [ -n "$line_b" ] && [ "$line_a" -lt "$line_b" ]; then
        pass "$3"
    else
        fail "$3: '$1' at line ${line_a:-none}, '$2' at line ${line_b:-none}"
    fi
}

expect_says() { # command-output want label
    if [ "$1" = "$2" ]; then
        pass "$3"
    else
        fail "$3: got '$1', wanted '$2'"
    fi
}

# --- channel fixtures ------------------------------------------------------

# A channel directory shaped exactly like a published one: manifest.json,
# SHA256SUMS, and one tarball holding a single `mapbox` at its root. The
# manifest lists three other targets as well, so the sed that picks out `file`
# and `sha256` has to choose between entries rather than match the only one
# there. One of the decoys is the Windows `.zip`, sitting immediately after
# the entry being read: extensions are no longer uniform across targets, and
# the neighbor a greedy match would bleed into is the one that proves it does
# not.
make_channel() { # dir version [bad-sha|broken]
    channel_dir="$1"
    channel_version="$2"
    channel_flaw="${3:-}"

    mkdir -p "${channel_dir}/build"
    if [ "$channel_flaw" = broken ]; then
        # Installs fine and then does not run, the shape of an artifact built
        # for another platform.
        printf '#!/bin/sh\nexit 1\n' >"${channel_dir}/build/mapbox"
    else
        cat >"${channel_dir}/build/mapbox" <<EOF
#!/bin/sh
case "\${1:-}" in
    --version) echo "mapbox ${channel_version#v}" ;;
    *) echo "fake mapbox: \$*" ;;
esac
EOF
    fi
    chmod 755 "${channel_dir}/build/mapbox"

    tarball="mapbox-${channel_version}-${TARGET}.tar.gz"
    tar czf "${channel_dir}/${tarball}" -C "${channel_dir}/build" mapbox
    rm -rf "${channel_dir}/build"

    real_sha="$(sha256_of "${channel_dir}/${tarball}")"
    case "$channel_flaw" in
        '' | broken) listed_sha="$real_sha" ;;
        *) listed_sha="$channel_flaw" ;;
    esac

    cat >"${channel_dir}/manifest.json" <<EOF
{
  "version": "${channel_version#v}",
  "commit": "0000000000000000000000000000000000000000",
  "released": "2026-01-01T00:00:00Z",
  "artifacts": {
    "sparc64-unknown-none": {
      "file": "mapbox-${channel_version}-sparc64-unknown-none.tar.gz",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
    },
    "${TARGET}": {
      "file": "${tarball}",
      "sha256": "${listed_sha}"
    },
    "x86_64-pc-windows-msvc": {
      "file": "mapbox-${channel_version}-x86_64-pc-windows-msvc.zip",
      "sha256": "2222222222222222222222222222222222222222222222222222222222222222"
    },
    "vax-unknown-none": {
      "file": "mapbox-${channel_version}-vax-unknown-none.tar.gz",
      "sha256": "1111111111111111111111111111111111111111111111111111111111111111"
    }
  }
}
EOF
    printf '%s  %s\n' "$real_sha" "$tarball" >"${channel_dir}/SHA256SUMS"
}

CHANNEL="${ROOT}/channel"
make_channel "${CHANNEL}/latest" v9.9.9
make_channel "${CHANNEL}/v0.1.0-dev.abc1234" v0.1.0-dev.abc1234

# A channel whose manifest advertises a checksum the tarball does not have.
BAD_SHA="${ROOT}/bad-sha-channel"
make_channel "${BAD_SHA}/latest" v9.9.9 \
    deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef

# A channel whose artifact checksums out and then will not run.
BROKEN="${ROOT}/broken-channel"
make_channel "${BROKEN}/latest" v9.9.9 broken

# A channel that serves no artifact for this machine.
FOREIGN="${ROOT}/foreign-channel"
mkdir -p "${FOREIGN}/latest"
cat >"${FOREIGN}/latest/manifest.json" <<EOF
{
  "version": "9.9.9",
  "artifacts": {
    "sparc64-unknown-none": { "file": "x.tar.gz", "sha256": "00" }
  }
}
EOF

# --- command shims ---------------------------------------------------------
#
# Stand-ins for what install.sh looks for on PATH. Cases opt in one at a time
# so nothing leaks between them, and none of them touch a real environment.

mkdir -p "$SHIMS"

# `pipx install` without the mutation: drops a `tilesets` beside itself, so
# the success path — including reading the version back — is what runs.
cat >"${SHIMS}/pipx" <<'EOF'
#!/bin/sh
echo "fake pipx: $*"
cat >"$(dirname "$0")/tilesets" <<'INNER'
#!/bin/sh
echo "tilesets, version 1.11.0"
INNER
chmod 755 "$(dirname "$0")/tilesets"
EOF

# A pipx that reports success and installs nothing reachable, the shape of a
# pip --user install landing in a directory that is not on PATH.
cat >"${SHIMS}/pipx-silent" <<'EOF'
#!/bin/sh
echo "fake pipx: $*"
EOF

cat >"${SHIMS}/tilesets" <<'EOF'
#!/bin/sh
echo "tilesets, version 1.11.0"
EOF

# python3 stand-ins. install.sh runs two `-c` probes — the 3.10+ version gate
# and the PEP 668 marker — so these tell them apart by what the code they are
# handed mentions. `-m pip` is the install, which must never reach a real
# interpreter.
cat >"${SHIMS}/python3-310" <<'EOF'
#!/bin/sh
case "${1:-} ${2:-}" in
    *EXTERNALLY-MANAGED*) exit 1 ;;      # not managed: pip --user is allowed
    -c*) exit 0 ;;                       # 3.10 or newer
    -m*)
        echo "fake python3 $*"
        cat >"$(dirname "$0")/tilesets" <<'INNER'
#!/bin/sh
echo "tilesets, version 1.11.0"
INNER
        chmod 755 "$(dirname "$0")/tilesets"
        ;;
esac
EOF

# 3.10+, and managed by the OS: `pip install --user` is refused outright, so
# install.sh must not reach for it. Debian 12 and Ubuntu 23.04 onward.
cat >"${SHIMS}/python3-310-managed" <<'EOF'
#!/bin/sh
case "${1:-} ${2:-}" in
    *EXTERNALLY-MANAGED*) exit 0 ;;
    -c*) exit 0 ;;
    -m*) echo "fake python3 $*" ;;
esac
EOF

cat >"${SHIMS}/python3-39" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -c) exit 1 ;;
    *) echo "fake python3: $*" ;;
esac
EOF

cat >"${SHIMS}/uname-unsupported" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -s) echo Plan9 ;;
    -m) echo vax ;;
esac
EOF

# A Linux x86_64 machine, whatever this is running on. The WSL check sits in
# the Linux branch of detect_target, and the marker is built before the
# manifest is fetched — so the request is recorded whether or not the test
# channel happens to hold an artifact for that triple.
cat >"${SHIMS}/uname-linux" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -s) echo Linux ;;
    -m) echo x86_64 ;;
esac
EOF

# An Intel Mac, whatever this is running on: the Rosetta check sits in the
# Darwin branch, and only the x86_64 triple can ever reach it.
cat >"${SHIMS}/uname-darwin-intel" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -s) echo Darwin ;;
    -m) echo x86_64 ;;
esac
EOF

# The two answers `sysctl -n sysctl.proc_translated` gives. A machine with no
# such key at all is the third case, and needs no shim: it is what every
# non-macOS host running this suite already does.
cat >"${SHIMS}/sysctl-translated" <<'EOF'
#!/bin/sh
echo 1
EOF

cat >"${SHIMS}/sysctl-native" <<'EOF'
#!/bin/sh
echo 0
EOF

# What Git Bash reports. MSYS2 and Cygwin differ only in the prefix.
cat >"${SHIMS}/uname-windows" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -s) echo MINGW64_NT-10.0-22631 ;;
    -m) echo x86_64 ;;
esac
EOF

# curl, recorded. It appends its arguments to $MAPBOX_TEST_CURL_LOG and then
# does the fetch for real, so the case still installs — a shim that answered
# by itself would prove the arguments and nothing else. The real curl is
# resolved here, by absolute path, because this shim *is* the `curl` on the
# case's PATH.
REAL_CURL="$(PATH="$SAFE_PATH" command -v curl)"
cat >"${SHIMS}/curl-recording" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >>"\${MAPBOX_TEST_CURL_LOG:-/dev/null}"
exec ${REAL_CURL} "\$@"
EOF

chmod 755 "${SHIMS}"/*

# --- running install.sh ----------------------------------------------------

# Runs the installer the way `curl … | sh` does — the script arrives on stdin,
# never as a file argument — in a new session, so /dev/tty cannot be opened.
# Combined output lands in $OUT and the exit status is returned. A run that
# outlives the timeout comes back 124, which is how a prompt that blocks with
# nobody to answer it shows up as a failure rather than a hang.
run_piped() {
    set +e
    MAPBOX_TEST_PATH="$PATH" "$PYTHON3" -c '
import os, signal, subprocess, sys, threading

timeout, stdin_path = float(sys.argv[1]), sys.argv[2]
env = dict(os.environ, PATH=os.environ["MAPBOX_TEST_PATH"])
with open(stdin_path, "rb") as script:
    child = subprocess.Popen(
        sys.argv[3:],
        stdin=script,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
        env=env,
    )
timed_out = []


def give_up():
    timed_out.append(True)
    os.killpg(os.getpgid(child.pid), signal.SIGKILL)


watchdog = threading.Timer(timeout, give_up)
watchdog.start()
try:
    sys.stdout.buffer.write(child.stdout.read())
    child.wait()
finally:
    watchdog.cancel()
sys.exit(124 if timed_out else child.returncode)
' 90 "$INSTALLER" sh >"$OUT" 2>&1
    run_status=$?
    set -e
    return $run_status
}

# The same thing with a terminal attached: script(1) allocates a pty, so
# /dev/tty opens and the prompt appears, while the installer itself still
# arrives on stdin through a pipe. $1 is typed at the prompt.
#
# That answer goes into a pipe which is deliberately never closed. script(1)
# forwards an end-of-input to the pty as soon as its own stdin ends, and that
# arrives long before the installer reaches the prompt — leaving `read` at EOF
# with the typed answer still sitting in the buffer.
run_interactive() {
    # Read before `set --` replaces the positional parameters.
    typed="$1"
    if [ "$(uname -s)" = Darwin ]; then
        set -- script -q /dev/null sh -c "cat '${INSTALLER}' | sh"
    else
        set -- script -qec "cat '${INSTALLER}' | sh" /dev/null
    fi
    set +e
    MAPBOX_TEST_TYPED="$typed" MAPBOX_TEST_PATH="$PATH" "$PYTHON3" -c '
import os, signal, subprocess, sys, threading

child = subprocess.Popen(
    sys.argv[2:],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
    start_new_session=True,
    env=dict(os.environ, PATH=os.environ["MAPBOX_TEST_PATH"]),
)
timed_out = []


def give_up():
    timed_out.append(True)
    os.killpg(os.getpgid(child.pid), signal.SIGKILL)


watchdog = threading.Timer(float(sys.argv[1]), give_up)
watchdog.start()
try:
    child.stdin.write((os.environ["MAPBOX_TEST_TYPED"] + "\n").encode())
    child.stdin.flush()
    sys.stdout.buffer.write(child.stdout.read())
    child.wait()
finally:
    watchdog.cancel()
sys.exit(124 if timed_out else child.returncode)
' 90 "$@" >"$OUT" 2>&1
    run_status=$?
    set -e
    return $run_status
}

# Each case gets its own install dir, its own shim directory, and a PATH that
# contains neither — so the PATH advice and the shadowing note are testable,
# and no case can see what another one installed.
new_case_env() { # case-name
    CASE_DIR="${ROOT}/cases/$1"
    BIN_DIR="${CASE_DIR}/bin"
    CASE_SHIMS="${CASE_DIR}/shims"
    rm -rf "${CASE_DIR:?}"
    mkdir -p "$BIN_DIR" "$CASE_SHIMS"
    MAPBOX_CLI_BASE_URL="file://${CHANNEL}"
    MAPBOX_INSTALL_DIR="$BIN_DIR"
    PATH="${CASE_SHIMS}:${SAFE_PATH}"
    unset MAPBOX_CLI_VERSION MAPBOX_CLI_AUTH MAPBOX_INSTALL_TILESETS MAPBOX_TILESETS_CLI
    unset MAPBOX_CLI_INSTALL_SOURCE
    # A developer with either of these set in their own shell would otherwise
    # turn every marker case into a failure that looks like the marker broke.
    unset DISABLE_TELEMETRY MAPBOX_CLI_NO_TELEMETRY
    export MAPBOX_CLI_BASE_URL MAPBOX_INSTALL_DIR PATH
}

shim() { # shim-name [name-on-path]
    cp "${SHIMS}/$1" "${CASE_SHIMS}/${2:-$1}"
}

# --- installing the binary -------------------------------------------------

start 'installs the binary the manifest names'
new_case_env happy
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_file "${BIN_DIR}/mapbox" 'the binary is in the install dir'
expect_out 'Installed mapbox 9.9.9' 'reports the version it ran, not the one it was promised'
expect_out "${BIN_DIR}/mapbox" 'reports the path'
expect_says "$("${BIN_DIR}/mapbox" --version)" 'mapbox 9.9.9' 'the installed binary runs'
ls -a "$BIN_DIR" >"$OUT" 2>&1
expect_no_out '.mapbox.install.' 'leaves no staging file behind'

start 'every request it makes says it came from the installer'
new_case_env user-agent
export MAPBOX_INSTALL_TILESETS=no
shim curl-recording curl
CURL_LOG="${CASE_DIR}/curl-args"
: >"$CURL_LOG"
export MAPBOX_TEST_CURL_LOG="$CURL_LOG"
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
requests="$(grep -c . "$CURL_LOG" 2>/dev/null || true)"
# The triple without its closing paren, so this still passes when the suite is
# run on WSL itself, where `; wsl` is the correct answer.
marked="$(grep -cF -- "-A ${INSTALLER_UA} (${TARGET}" "$CURL_LOG" 2>/dev/null || true)"
# Two at least: the manifest and the artifact. Every one of them, not most —
# an unmarked request is a download that cannot be attributed.
if [ "${requests:-0}" -ge 2 ] && [ "${requests:-0}" = "${marked:-0}" ]; then
    pass "all ${requests} requests carry the User-Agent, target triple and all"
else
    fail "all requests carry the User-Agent: ${marked:-0} of ${requests:-0} did"
    sed 's/^/      | /' "$CURL_LOG"
fi
unset MAPBOX_TEST_CURL_LOG

start 'a WSL install says so rather than passing for Linux'
new_case_env wsl
export MAPBOX_INSTALL_TILESETS=no
export WSL_DISTRO_NAME=Ubuntu-24.04
shim curl-recording curl
shim uname-linux uname
CURL_LOG="${CASE_DIR}/curl-args"
: >"$CURL_LOG"
export MAPBOX_TEST_CURL_LOG="$CURL_LOG"
# The exit status is deliberately not asserted: on an x86_64 Linux host this
# resolves the host's own target and installs, and anywhere else the channel
# holds no artifact for it. The request went out before either could happen,
# which is the whole of what this case is about.
run_piped || true
expect_in_file "$CURL_LOG" "-A ${INSTALLER_UA} (x86_64-unknown-linux-musl; wsl)" \
    'names wsl beside the triple'
unset MAPBOX_TEST_CURL_LOG WSL_DISTRO_NAME

start 'a Rosetta install says so rather than passing for an Intel Mac'
new_case_env rosetta
export MAPBOX_INSTALL_TILESETS=no
shim curl-recording curl
shim uname-darwin-intel uname
shim sysctl-translated sysctl
CURL_LOG="${CASE_DIR}/curl-args"
: >"$CURL_LOG"
export MAPBOX_TEST_CURL_LOG="$CURL_LOG"
# Status-agnostic for the reason the WSL case is: the triple this resolves has
# an artifact in the channel only when it happens to be the host's own. The
# request is made before that can matter.
run_piped || true
expect_in_file "$CURL_LOG" "-A ${INSTALLER_UA} (x86_64-apple-darwin; rosetta)" \
    'names rosetta beside the triple'
# The other direction, which is what makes the check worth having: a native
# process must not be marked, or every Intel Mac reads as a translated one.
: >"$CURL_LOG"
shim sysctl-native sysctl
run_piped || true
expect_in_file "$CURL_LOG" "-A ${INSTALLER_UA} (x86_64-apple-darwin)" \
    'a native Intel Mac is just the triple'
expect_not_in_file "$CURL_LOG" 'rosetta' 'and says nothing about Rosetta'
unset MAPBOX_TEST_CURL_LOG

start 'MAPBOX_CLI_INSTALL_SOURCE names what did the installing'
new_case_env install-source
export MAPBOX_INSTALL_TILESETS=no
export MAPBOX_CLI_INSTALL_SOURCE='onboarding script; rm -rf /'
shim curl-recording curl
CURL_LOG="${CASE_DIR}/curl-args"
: >"$CURL_LOG"
export MAPBOX_TEST_CURL_LOG="$CURL_LOG"
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_in_file "$CURL_LOG" 'src/onboardingscriptrm-rf' \
    'the tag rides along, with everything a header should not carry dropped'
unset MAPBOX_TEST_CURL_LOG MAPBOX_CLI_INSTALL_SOURCE

start 'DISABLE_TELEMETRY keeps it down to the product token'
new_case_env telemetry-off
export MAPBOX_INSTALL_TILESETS=no
export MAPBOX_CLI_INSTALL_SOURCE=dockerfile
export DISABLE_TELEMETRY=1
shim curl-recording curl
CURL_LOG="${CASE_DIR}/curl-args"
: >"$CURL_LOG"
export MAPBOX_TEST_CURL_LOG="$CURL_LOG"
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0 — the install is not what is being switched off'
expect_in_file "$CURL_LOG" "-A ${INSTALLER_UA} " 'still names the installer'
expect_not_in_file "$CURL_LOG" "${INSTALLER_UA} (" 'no platform rides behind it'
expect_not_in_file "$CURL_LOG" ' src/' 'and no source tag either'
# The other half, and the reason a bare "is it set" check will not do: `0` is
# someone declining the opt-out, not taking it.
: >"$CURL_LOG"
export DISABLE_TELEMETRY=0
run_piped || true
expect_in_file "$CURL_LOG" "-A ${INSTALLER_UA} (${TARGET}" 'DISABLE_TELEMETRY=0 is not an opt-out'
expect_in_file "$CURL_LOG" ' src/dockerfile' 'and the tag comes back with it'
unset MAPBOX_TEST_CURL_LOG DISABLE_TELEMETRY MAPBOX_CLI_INSTALL_SOURCE

start 'MAPBOX_CLI_NO_TELEMETRY is honored, and outranks the old name'
new_case_env telemetry-new-name
export MAPBOX_INSTALL_TILESETS=no
export MAPBOX_CLI_INSTALL_SOURCE=dockerfile
shim curl-recording curl
CURL_LOG="${CASE_DIR}/curl-args"
export MAPBOX_TEST_CURL_LOG="$CURL_LOG"

# The documented name, which the binary reads and this script honors too.
: >"$CURL_LOG"
export MAPBOX_CLI_NO_TELEMETRY=1
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0 — the install is not what is being switched off'
expect_in_file "$CURL_LOG" "-A ${INSTALLER_UA} " 'still names the installer'
expect_not_in_file "$CURL_LOG" "${INSTALLER_UA} (" 'no platform rides behind it'
expect_not_in_file "$CURL_LOG" ' src/' 'and no source tag either'

# Both set, disagreeing. The new name is the documented one, so an explicit
# `0` on it beats a `DISABLE_TELEMETRY=1` left in an image from before the
# rename — otherwise the old variable could never be retired.
: >"$CURL_LOG"
export MAPBOX_CLI_NO_TELEMETRY=0
export DISABLE_TELEMETRY=1
run_piped || true
expect_in_file "$CURL_LOG" "-A ${INSTALLER_UA} (${TARGET}" 'the new name wins when the two disagree'

# And the other direction, so precedence is pinned rather than implied.
: >"$CURL_LOG"
export MAPBOX_CLI_NO_TELEMETRY=1
export DISABLE_TELEMETRY=0
run_piped || true
expect_not_in_file "$CURL_LOG" "${INSTALLER_UA} (" 'and wins in the opt-out direction too'
unset MAPBOX_TEST_CURL_LOG MAPBOX_CLI_NO_TELEMETRY DISABLE_TELEMETRY MAPBOX_CLI_INSTALL_SOURCE

start 'both installers send the same marker'
# The one thing about this marker that cannot be checked by watching a request:
# install.sh and install.ps1 each carry their own copy, because each is served
# and run alone, and a bump applied to one and not the other is invisible to
# either suite — every assertion on both sides still passes, and the only
# symptom is two shapes in the access logs. So compare the declarations
# directly. Reading the .ps1 needs no PowerShell; it is a text file here.
ps_ua="$(sed -n 's/^ *[$]UserAgent = .\(mapbox-cli-install\/[0-9][0-9]*\).$/\1/p' "$PS_INSTALLER")"
# What a failure here needs to show is the two declarations, not whatever the
# last case that ran an installer printed.
{
    echo "install.sh   ${INSTALLER_UA}"
    echo "install.ps1  ${ps_ua:-<not found>}"
} >"$OUT"
if [ -z "$ps_ua" ]; then
    fail "install.ps1 declares a mapbox-cli-install/<n> User-Agent"
else
    expect_says "$ps_ua" "$INSTALLER_UA" 'install.ps1 sends what install.sh sends'
fi

start 'a checksum that does not match installs nothing'
new_case_env bad-sha
export MAPBOX_CLI_BASE_URL="file://${BAD_SHA}"
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 1 "$status" 'exits 1'
expect_out 'checksum mismatch' 'says the checksum did not match'
expect_out 'deadbeef' 'shows what it expected'
expect_out 'nothing was installed' 'says nothing was installed'
expect_no_file "${BIN_DIR}/mapbox" 'and nothing was'

start 'an artifact that checksums out and then will not run'
new_case_env broken-binary
export MAPBOX_CLI_BASE_URL="file://${BROKEN}"
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 1 "$status" 'exits 1'
expect_out 'it does not run here' 'checks by running --version rather than assuming'
expect_out "$TARGET" 'names the target it resolved'

start 'a manifest with no artifact for this machine'
new_case_env no-artifact
export MAPBOX_CLI_BASE_URL="file://${FOREIGN}"
run_piped && status=0 || status=$?
expect_status 1 "$status" 'exits 1'
expect_out "lists no artifact for ${TARGET}" 'names the target'
expect_no_file "${BIN_DIR}/mapbox" 'nothing was installed'

start 'a channel that is not there'
new_case_env missing-channel
export MAPBOX_CLI_BASE_URL="file://${ROOT}/nowhere"
run_piped && status=0 || status=$?
expect_status 1 "$status" 'exits 1'
expect_out 'could not read' 'says which URL it could not read'

start 'MAPBOX_CLI_VERSION pins a version directory'
new_case_env pinned
export MAPBOX_CLI_VERSION=v0.1.0-dev.abc1234
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'Installed mapbox 0.1.0-dev.abc1234' 'installs that exact version'
expect_out 'channel  v0.1.0-dev.abc1234' 'names the channel it resolved'

# The channel's directories carry a leading `v`. Every place a person reads a
# version from — `mapbox --version`, CHANGELOG.md, Cargo.toml — shows it
# without one, so the spelling somebody copies is the one that has to work.
# It used to 403, which reads as "not allowed" rather than "no such version".
start 'MAPBOX_CLI_VERSION accepts a version without the leading v'
new_case_env pinned-bare
export MAPBOX_CLI_VERSION=0.1.0-dev.abc1234
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'Installed mapbox 0.1.0-dev.abc1234' 'installs that exact version'
expect_out 'channel  v0.1.0-dev.abc1234' 'and resolved the v-prefixed directory'

# `latest` starts with a letter, so nothing is prepended to it. Pinning this
# wrong would break the default install rather than an edge case.
start 'a channel name that is not a version is left alone'
new_case_env pinned-latest
export MAPBOX_CLI_VERSION=latest
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'channel  latest' 'asked for latest, not vlatest'

start 'an unsupported platform stops before downloading'
new_case_env unsupported
shim uname-unsupported uname
run_piped && status=0 || status=$?
expect_status 1 "$status" 'exits 1'
expect_out 'no prebuilt binary for Plan9 vax' 'names the platform'
expect_no_file "${BIN_DIR}/mapbox" 'nothing was installed'

start 'Git Bash is pointed at install.ps1 and at WSL, not a kernel name'
new_case_env windows
shim uname-windows uname
run_piped && status=0 || status=$?
expect_status 1 "$status" 'exits 1'
expect_out 'does not run on Windows' 'says which of the two it is: no installer here, not no build'
expect_out 'install.ps1 | iex' 'hands over the one that does run there'
expect_out "${CHANNEL}/install.ps1" 'pointing at this channel, not a hardcoded one'
expect_out 'use WSL' 'and at the other thing that works'
expect_no_out 'no Windows build' 'does not still claim there is no build'
expect_no_out 'no prebuilt binary for MINGW64_NT' 'does not just echo the kernel name'
expect_no_file "${BIN_DIR}/mapbox" 'nothing was installed'

start 'the Windows message names the credential rather than echoing it'
new_case_env windows-gated
shim uname-windows uname
MAPBOX_CLI_AUTH='someone:hunter2'
export MAPBOX_CLI_AUTH
run_piped && status=0 || status=$?
unset MAPBOX_CLI_AUTH
expect_status 1 "$status" 'exits 1'
# A PowerShell variable, in a shell script: literal on purpose.
# shellcheck disable=SC2016
expect_out '$env:MAPBOX_CLI_AUTH' 'names the variable the other shell needs'
expect_no_out 'hunter2' 'and does not print the credential into a terminal log'

# install.ps1's counterpart is the 'unsubstituted' case in test-install.ps1,
# and the two messages are meant to say the same thing.
start 'the copy in the repository, with no channel substituted in'
new_case_env unsubstituted
unset MAPBOX_CLI_BASE_URL
run_piped && status=0 || status=$?
expect_status 1 "$status" 'exits 1'
expect_out 'no channel to install from' 'says what is wrong'
expect_out 'MAPBOX_CLI_BASE_URL' 'names the way to point it at one'
expect_no_out 'Could not resolve host' 'stops before curl is asked to resolve a placeholder'
expect_no_file "${BIN_DIR}/mapbox" 'nothing was installed'

start 'reinstalling reports the version it replaced'
new_case_env upgrade
printf '#!/bin/sh\necho "mapbox 0.0.1"\n' >"${BIN_DIR}/mapbox"
chmod 755 "${BIN_DIR}/mapbox"
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'replaced mapbox 0.0.1' 'reports old to new'
expect_out 'Installed mapbox 9.9.9' 'reports the new version'

start 'a mapbox from somewhere else is reported, not replaced'
new_case_env elsewhere
OTHER="${CASE_DIR}/homebrew-bin"
mkdir -p "$OTHER"
printf '#!/bin/sh\necho "mapbox 0.0.2"\n' >"${OTHER}/mapbox"
chmod 755 "${OTHER}/mapbox"
export PATH="${OTHER}:${BIN_DIR}:${PATH}"
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out "still resolves to ${OTHER}/mapbox" 'names the other binary'
expect_out 'did not touch it' 'says it left the other one alone'
expect_no_out 'is not on your PATH' 'does not also claim the install dir is off PATH'
expect_says "$("${OTHER}/mapbox" --version)" 'mapbox 0.0.2' 'the other binary is untouched'

start 'a mapbox from somewhere else, with the install dir not on PATH'
new_case_env elsewhere-off-path
OTHER="${CASE_DIR}/homebrew-bin"
mkdir -p "$OTHER"
printf '#!/bin/sh\necho "mapbox 0.0.2"\n' >"${OTHER}/mapbox"
chmod 755 "${OTHER}/mapbox"
export PATH="${OTHER}:${PATH}"
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'is not on your PATH' 'says the install dir is not on PATH'
expect_out "still resolves to ${OTHER}/mapbox" 'and names the other binary too'

start 'a mapbox further down PATH is named, with the way to clear the cache'
new_case_env behind
OTHER="${CASE_DIR}/cargo-bin"
mkdir -p "$OTHER"
printf '#!/bin/sh\necho "mapbox 0.0.2"\n' >"${OTHER}/mapbox"
chmod 755 "${OTHER}/mapbox"
# The install dir wins, so `command -v` finds nothing wrong and the note above
# stays quiet — while a shell that has already run the other copy goes on
# reporting 0.0.2. That gap is what this case pins.
export PATH="${BIN_DIR}:${OTHER}:${PATH}"
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out "another mapbox at ${OTHER}/mapbox" 'names the other binary'
expect_out 'hash -r' 'gives the command that drops the cached path'
expect_out 'prints mapbox 9.9.9' 'says what a new shell will report'
expect_no_out 'still resolves to' 'does not claim the other one wins'
expect_no_out 'is not on your PATH' 'and nothing about PATH'
expect_says "$("${OTHER}/mapbox" --version)" 'mapbox 0.0.2' 'the other binary is untouched'

start 'an install dir that is on PATH gets no PATH advice'
new_case_env on-path
export PATH="${BIN_DIR}:${PATH}"
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_no_out 'is not on your PATH' 'says nothing about PATH'
expect_no_out 'still resolves to' 'and nothing about shadowing'
expect_no_out 'another mapbox at' 'and nothing about a second copy, because there is none'

start 'an install dir that is not on PATH is named, with the line to add'
new_case_env off-path
export SHELL=/bin/zsh
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out "${BIN_DIR} is not on your PATH" 'names the directory'
expect_out "export PATH=\"${BIN_DIR}:\$PATH\"" 'prints the exact line'
# The literal ~ is the point: it is advice to read, not a path to open.
# shellcheck disable=SC2088
expect_out '~/.zshrc' 'names the file for the shell in use'
unset SHELL

start 'an install dir that does not exist yet'
new_case_env fresh-dir
export MAPBOX_INSTALL_DIR="${BIN_DIR}/nested/deeper"
export MAPBOX_INSTALL_TILESETS=no
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_file "${BIN_DIR}/nested/deeper/mapbox" 'created the directory and installed into it'

start 'an install dir that exists but cannot be written'
new_case_env readonly
if [ "$(id -u)" = 0 ]; then
    pass 'skipped: running as root, which can write anywhere'
else
    chmod 500 "$BIN_DIR"
    run_piped && status=0 || status=$?
    chmod 700 "$BIN_DIR"
    expect_status 1 "$status" 'exits 1'
    expect_out 'is not writable' 'says the directory is not writable'
    expect_out 'MAPBOX_INSTALL_DIR' 'points at the override'
    expect_out 'never calls sudo on your behalf' 'does not offer to escalate'
    expect_no_file "${BIN_DIR}/mapbox" 'nothing was installed'
fi

# --- the Tilesets CLI ------------------------------------------------------

start 'no terminal: the prompt is skipped, not answered yes'
new_case_env no-tty
shim pipx
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0 rather than blocking (124 would be a hang)'
expect_file "${BIN_DIR}/mapbox" 'mapbox is installed'
expect_out 'The Mapbox Tilesets CLI is not installed' 'explains what is missing'
expect_out 'pipx install mapbox-tilesets' 'prints the instructions instead of asking'
expect_out 'MAPBOX_TILESETS_CLI' 'mentions the override, as the CLI does'
expect_no_out 'Install it as well?' 'does not ask when nobody can answer'
expect_no_out 'fake pipx' 'installs nothing unasked'

start 'a terminal, answered no'
new_case_env tty-no
shim pipx
run_interactive n && status=0 || status=$?
expect_status 0 "$status" 'a declined extra is not an install failure'
expect_file "${BIN_DIR}/mapbox" 'mapbox is installed'
expect_out 'Install it as well?' 'asks'
expect_out 'mapbox is installed and ready to use' 'says mapbox is done before asking anything'
expect_out 'the rest of this is optional' 'calls the extra optional'
expect_order 'mapbox is installed and ready to use' 'Install it as well?' \
    'reports the finished install before the question, not after'
expect_says "$("${BIN_DIR}/mapbox" --version)" 'mapbox 9.9.9' 'and mapbox runs after answering no'
expect_out 'pipx install mapbox-tilesets' 'falls back to printing the instructions'
expect_no_out 'fake pipx' 'runs no installer'

start 'a terminal, answered with a bare newline'
new_case_env tty-default
shim pipx
run_interactive '' && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'Install it as well?' 'asks'
expect_no_out 'fake pipx' 'the default is no'

start 'a terminal, answered yes'
new_case_env tty-yes
shim pipx
run_interactive y && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_file "${BIN_DIR}/mapbox" 'mapbox is installed'
expect_out 'Install it as well?' 'asks'
expect_out 'fake pipx: install mapbox-tilesets' 'prefers pipx when it is there'
expect_out 'Tilesets CLI: tilesets, version 1.11.0' 'reads the version back'

start 'MAPBOX_INSTALL_TILESETS=yes answers ahead of time'
new_case_env preanswered-yes
shim pipx
export MAPBOX_INSTALL_TILESETS=yes
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_no_out 'Install it as well?' 'does not ask'
expect_out 'fake pipx: install mapbox-tilesets' 'installs with no terminal in sight'

start 'MAPBOX_INSTALL_TILESETS with a value that is neither yes nor no'
new_case_env preanswered-junk
shim pipx
export MAPBOX_INSTALL_TILESETS=maybe
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'neither yes nor no' 'says the value was not understood'
expect_no_out 'fake pipx' 'installs nothing on an unclear answer'

start 'no pipx, but a Python 3.10+ to fall back on'
new_case_env pip-fallback
shim python3-310 python3
export MAPBOX_INSTALL_TILESETS=yes
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'pipx is not installed' 'says why it is not using pipx'
expect_out 'fake python3 -m pip install --user mapbox-tilesets' 'falls back to pip --user'
expect_out 'Tilesets CLI: tilesets, version 1.11.0' 'reads the version back'

start 'no pipx and an OS-managed python3: refuse rather than run a doomed pip'
new_case_env managed-python
shim python3-310-managed python3
export MAPBOX_INSTALL_TILESETS=yes
run_piped && status=0 || status=$?
expect_status 0 "$status" 'still exits 0 — mapbox itself installed'
expect_file "${BIN_DIR}/mapbox" 'mapbox is installed'
expect_out 'managed by your OS (PEP 668)' 'names why pip is not an option'
expect_out 'apt install pipx' 'points at the thing that does work there'
expect_no_out 'fake python3 -m pip' 'does not run pip at all'
expect_no_out 'pipx is not installed; using pip instead' 'does not announce a fallback it will not take'

start 'no pipx and no Python 3.10+: refuse rather than guess'
new_case_env no-python
shim python3-39 python3
export MAPBOX_INSTALL_TILESETS=yes
run_piped && status=0 || status=$?
expect_status 0 "$status" 'still exits 0 — mapbox itself installed'
expect_file "${BIN_DIR}/mapbox" 'mapbox is installed'
expect_out 'Cannot install it here' 'refuses'
expect_out 'no Python 3.10+' 'says what is missing'
expect_out 'pipx install mapbox-tilesets' 'prints the instructions instead'
expect_no_out 'fake python3 -m pip' 'runs no installer'

start 'an install that reports success but lands off PATH'
new_case_env unreachable
shim pipx-silent pipx
export MAPBOX_INSTALL_TILESETS=yes
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'fake pipx: install mapbox-tilesets' 'ran the installer'
expect_out 'is not on your PATH' 'says the binary is not reachable'
expect_out 'MAPBOX_TILESETS_CLI' 'offers the override'

start 'a tilesets already on PATH is reported and nothing is asked'
new_case_env tilesets-present
shim tilesets
shim pipx
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out 'Tilesets CLI: tilesets, version 1.11.0' 'reports its version'
expect_no_out 'is not installed' 'says nothing else about it'
expect_no_out 'Install it as well?' 'does not ask'
expect_no_out 'fake pipx' 'installs nothing'

start 'MAPBOX_TILESETS_CLI is honored, set or broken'
new_case_env tilesets-override
shim pipx
mkdir -p "${CASE_DIR}/opt"
cp "${SHIMS}/tilesets" "${CASE_DIR}/opt/ts"
export MAPBOX_TILESETS_CLI="${CASE_DIR}/opt/ts"
run_piped && status=0 || status=$?
expect_status 0 "$status" 'exits 0'
expect_out "Tilesets CLI: ${CASE_DIR}/opt/ts (MAPBOX_TILESETS_CLI)" 'reports the override'
expect_no_out 'Install it as well?' 'does not offer to install over an override'
expect_no_out 'fake pipx' 'installs nothing'

export MAPBOX_TILESETS_CLI="${CASE_DIR}/opt/gone"
run_piped && status=0 || status=$?
expect_status 0 "$status" 'a broken override is not an install failure'
expect_out 'where there is no' 'says the override points nowhere'
expect_no_out 'fake pipx' 'does not install over a deliberate override'

# --- result ----------------------------------------------------------------

printf '\n'
if [ "$FAILURES" -eq 0 ]; then
    echo 'install.sh: all cases passed'
else
    echo "install.sh: ${FAILURES} failed assertion(s)"
    exit 1
fi
