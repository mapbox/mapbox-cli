# mapbox-cli

A command-line interface for Mapbox APIs. Commands are generated at build
time from OpenAPI specs, so they always match the specs.

```sh
brew install mapbox/tap/mapbox
mapbox auth login
mapbox styles list
```

## Contents

- [Install](#install)
  - [Homebrew](#homebrew)
  - [Install script](#install-script)
  - [Download the archive yourself](#download-the-archive-yourself)
  - [Build from source](#build-from-source)
  - [Uninstall](#uninstall)
- [Authentication](#authentication)
  - [Named profiles](#named-profiles)
- [Commands](#commands)
  - [API commands](#api-commands)
  - [Diagnostics and settings](#diagnostics-and-settings)
  - [Shell completion](#shell-completion)
  - [Tileset CLI](#tileset-cli)
- [For AI agents](#for-ai-agents)
  - [Quick start: Mapbox agent setup](#quick-start-mapbox-agent-setup)
  - [Agent skills](#agent-skills)
  - [Generate skills](#generate-skills)
  - [MCP servers](#mcp-servers)
- [Global options](#global-options)
  - [Dry runs](#dry-runs)
  - [Timeouts](#timeouts)
  - [Extra query parameters](#extra-query-parameters)
  - [Proxies](#proxies)
  - [Output format](#output-format)
  - [`--schema`](#--schema)
  - [Confirmation and `--yes`](#confirmation-and---yes)
- [Updates, history and logs](#updates-history-and-logs)
  - [Update notices](#update-notices)
  - [Command history](#command-history)
  - [Diagnostic logs](#diagnostic-logs)
- [Privacy](#privacy)
- [Contributing](#contributing)

## Install

Homebrew covers macOS and Linux. On Windows, use the install script or
download the archive.

### Homebrew

On macOS or Linux:

```sh
brew install mapbox/tap/mapbox
```

Update with `brew upgrade mapbox`. The formula also installs completions
for bash, zsh and fish, so there is nothing to set up under
[Shell completion](#shell-completion). The tap lives at
[mapbox/homebrew-tap](https://github.com/mapbox/homebrew-tap).

### Install script

The script detects your platform, checks a SHA-256 checksum, and installs
`mapbox` into `~/.local/bin`. No `sudo`, no admin rights:

```sh
curl -fsSL https://cli.mapbox.com/install.sh | sh
```

```powershell
irm https://cli.mapbox.com/install.ps1 | iex
```

Run it again to update. `MAPBOX_CLI_VERSION` pins a version instead of
taking the newest, with or without the leading `v`, so the version
`mapbox --version` prints can be pasted straight in:

```sh
curl -fsSL https://cli.mapbox.com/install.sh | MAPBOX_CLI_VERSION=0.3.0 sh
```

```powershell
$env:MAPBOX_CLI_VERSION = '0.3.0'; irm https://cli.mapbox.com/install.ps1 | iex
```

Both scripts put the install directory on your `PATH`: `install.sh` adds a
line to your shell profile, `install.ps1` edits the user `PATH`. Set
`MAPBOX_NO_MODIFY_PATH=1` to skip that and only print what to add.

`MAPBOX_INSTALL_DIR` chooses where the binary lands. The scripts' sources
are [`scripts/install.sh`](./scripts/install.sh) and
[`scripts/install.ps1`](./scripts/install.ps1).

If the script finds a coding agent on the machine ([Claude Code](#agent-skills)
and the rest of the fifteen `agent-skills` supports), it asks, once, whether
to set up this CLI's own [skill](#generate-skills) and the Mapbox Agent
Skills library for it, naming the agent before it writes anything. Answer no,
or run the script somewhere with no terminal to ask on (CI, a container), and
it does neither, printing the two commands to run by hand instead.

It then asks, separately, whether to add the [Mapbox MCP servers](#mcp-servers)
to each coding agent it finds that can take one (Claude Code, Codex, VS Code,
Cursor), the same thing `mapbox mcp install --global` does. It asks only when
there is something left to add, with the same rules: no terminal means no.
Set `MAPBOX_CLI_NO_AGENT_SETUP=1` to skip both questions.

On macOS and Linux, `install.sh` also offers to install the
[Tilesets CLI](https://github.com/mapbox/tilesets-cli), which only
`mapbox tilesets-cli` needs. `MAPBOX_INSTALL_TILESETS=yes` or `no` answers
that question in advance; with no terminal, the answer is no. To install
without being asked anything:

```sh
curl -fsSL https://cli.mapbox.com/install.sh | MAPBOX_CLI_NO_AGENT_SETUP=1 MAPBOX_INSTALL_TILESETS=no sh
```

`install.sh` records each run in `~/.local/state/mapbox-cli/install.log`
(under `$XDG_STATE_HOME` when that is set): every step, and the full output of
each `mapbox` and Tilesets command it ran after installing the binary. The
previous run's log is kept as `install.log.1`.

### Download the archive yourself

If piping a script into a shell is not allowed where you work, the archives
are ordinary HTTP downloads, and `manifest.json` lists every target with its
checksum:

```sh
curl -fsSL https://cli.mapbox.com/latest/manifest.json
```

Six targets are published: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`,
`x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`. Pick yours, check it,
then extract:

```sh
version=v0.3.0
file=mapbox-${version}-aarch64-apple-darwin.tar.gz

curl -fsSLO "https://cli.mapbox.com/${version}/${file}"
curl -fsSL "https://cli.mapbox.com/${version}/SHA256SUMS" |
    grep "$file" | shasum -a 256 -c -

tar -xzf "$file"
mv mapbox ~/.local/bin/
```

On Windows the archive is a `.zip`, and PowerShell can read the manifest
directly:

```powershell
$version = 'v0.3.0'
$target  = 'x86_64-pc-windows-msvc'

$manifest = Invoke-RestMethod "https://cli.mapbox.com/$version/manifest.json"
$artifact = $manifest.artifacts.$target

Invoke-WebRequest "https://cli.mapbox.com/$version/$($artifact.file)" -OutFile $artifact.file
if ((Get-FileHash -Algorithm SHA256 $artifact.file).Hash -ine $artifact.sha256) {
    throw 'checksum mismatch'
}

Expand-Archive $artifact.file -DestinationPath .
```

This uses `Invoke-WebRequest` and `Get-FileHash` because Windows PowerShell
5.1, the edition that ships with Windows, aliases `curl` to
`Invoke-WebRequest`, and `sha256sum` isn't shipped outside WSL or Git Bash.

Use `latest` in place of the version for whatever is current. Each archive
holds one file, the `mapbox` executable.

The macOS builds are not code-signed with a Developer ID. `curl` attaches no
quarantine flag, which is why the commands above run, but a download through
a browser does, and Gatekeeper will refuse an unsigned binary that carries
one. Clear it with `xattr -d com.apple.quarantine mapbox`.

### Build from source

It needs [Rust via rustup](https://rustup.rs) and nothing else: no second
repository, no token, and no network beyond crates.io. The OpenAPI specs the
commands are generated from are vendored in `openapi/`.

```sh
cargo build --release
./target/release/mapbox --help
```

### Uninstall

```sh
mapbox uninstall
```

Removes only the `mapbox` binary. Credentials, profiles, and the separate
`tilesets` binary are untouched. Run
[`mapbox auth logout`](./docs/commands.md#mapbox-auth-logout) first if you
want those gone too. Asks for confirmation like any destructive command
(`--yes`/`MAPBOX_YES` skips it); `--dry-run` previews without deleting.

Installed with Homebrew? Use `brew uninstall mapbox` instead, so Homebrew
knows it is gone.

## Authentication

```sh
mapbox auth login     # opens a browser (OAuth/PKCE)
mapbox auth whoami    # reports which token the next command will use
mapbox auth logout    # removes stored credentials
```

`auth refresh` and `auth profiles` complete the set; see
[docs/commands.md](./docs/commands.md#auth).

Credentials live in `~/.mapbox` as plain JSON with locked-down file
permissions. There is no OS keychain integration. Override them with
`--token`/`--username` or `MAPBOX_ACCESS_TOKEN`/`MAPBOX_USERNAME`, and set
`MAPBOX_CONFIG_DIR` to move the whole store, which a container usually
wants.

### Named profiles

`--profile <name>` keeps a separate credential set per account:

```sh
mapbox auth login --profile android_app
mapbox --profile android_app styles list
mapbox auth profiles              # which profiles are actually stored
```

## Commands

### API commands

Each Mapbox API is a command group, with one subcommand per operation:

```sh
mapbox accounts <operation>
mapbox feedback <operation>
mapbox fonts <operation>
mapbox geocoder <operation>
mapbox search <operation>
mapbox sprites <operation>
mapbox static <operation>
mapbox styles <operation>
mapbox tilesets <operation>
```

`mapbox directions`, `mapbox isochrone`, and `mapbox map-matching` are the
exceptions: each API has a single operation, so there's a bare command
with no subcommand at all, the same shape `mapbox usage` already has, see
[docs/commands.md](./docs/commands.md) for their own parameters.

A command group is not the same thing as a spec file: which one an operation
belongs to is decided per operation. So `sprites` and `tilesets` are each
assembled from operations declared by the Styles, Raster Tiles and Vector
Tiles specs. `mapbox tilesets` is also unrelated to `mapbox tilesets-cli`,
which proxies to the separate Python tool.

An operation can nest one level deeper where a group reads better, as in
`mapbox styles draft get`, `draft update` and `draft delete`.

For example:

```sh
mapbox styles get <STYLE_ID>
mapbox styles create --data '{"name": "My Style", "version": 8, ...}'
```

Groups are assigned per operation, not per spec file, so `sprites` and
`tilesets` each collect operations from several specs. A few nest one level
deeper where that reads better, as in `mapbox styles draft get`.
`mapbox tilesets` is unrelated to [`mapbox tilesets-cli`](#tileset-cli).

[docs/commands.md](./docs/commands.md) lists every command with its
parameters and sample output.

Every request sends `User-Agent: mapbox-cli/<version>` and nothing else
about you or your machine. `MAPBOX_CLI_NO_TELEMETRY=1` keeps even future
markers out of that header.

### Diagnostics and settings

```sh
mapbox doctor                        # the token, proxies and settings the next command would use
mapbox usage                         # account usage per product, by day
mapbox config set update-check off   # a setting that persists across shells
mapbox history list                  # recent runs, newest first
```

`mapbox doctor` is the first thing to run when a command behaves
unexpectedly. It makes no request unless you pass `--verify`. See
[Doctor](./docs/commands.md#doctor), [Usage](./docs/commands.md#usage) and
[Config](./docs/commands.md#config) for details, and
[Command history](#command-history) below.

### Shell completion

Homebrew installs completions for you. Otherwise, `mapbox completion`
prints a script on stdout for you to put where your shell looks:

```sh
mapbox completion bash > ~/.local/share/bash-completion/completions/mapbox
mapbox completion zsh  > ~/.zfunc/_mapbox            # a directory on $fpath
mapbox completion fish > ~/.config/fish/completions/mapbox.fish
source <(mapbox completion bash)                     # this shell only
```

```powershell
mapbox completion powershell >> $PROFILE
```

It completes commands, subcommands and flags from this binary's own command
tree, so it always matches the build that printed it. Values such as style
IDs are not completed, since that would mean an API request mid-keystroke.

### Tileset CLI

```sh
mapbox tilesets-cli list <USERNAME>
mapbox tilesets-cli upload-source <USERNAME> <SOURCE_ID> data.geojson.ld
```

Forwards everything to the separately installed [Tilesets
CLI](https://github.com/mapbox/tilesets-cli):

```sh
pipx install mapbox-tilesets       # Python 3.10+
```

It uses the same token as every other command. `--output` and `--yes` don't
apply here; `tilesets` has its own flags, so use `--force`/`-f` for its
prompts. If a tileset command answers for the wrong account,
`mapbox auth whoami` shows which token is in play.

## For AI agents

### Quick start: Mapbox agent setup

Paste this into your coding agent to install this CLI, the Mapbox Agent
Skills and the Mapbox MCP servers in one go:

```text
Set up Mapbox for me by following https://cli.mapbox.com/agent-setup/prompt.md
```

The agent runs the steps itself and asks you only to sign in to Mapbox in
the browser. The instructions it follows are in
[site/agent-setup/prompt.md](./site/agent-setup/prompt.md).

To set things up by hand instead, there are two commands for two different
jobs: `agent-skills` installs guidance on using Mapbox, and
`generate-skills` writes a skill describing this CLI.

### Agent skills

```sh
mapbox agent-skills list        # what's published, and what's installed here
mapbox agent-skills install     # every skill, for whichever agents you have
mapbox agent-skills update      # re-install what's here, report what changed
mapbox agent-skills uninstall <NAME>
```

Installs the [Mapbox Agent Skills](https://github.com/mapbox/mapbox-agent-skills):
hand-written guidance for coding agents on cartography, token security, style
quality, geospatial operations and the mobile and web SDKs. No token or Node
needed.

By default it installs into this project for each supported agent it finds
on the machine, including Claude Code, Codex, Cursor and GitHub Copilot.
`--agent` picks one, `--global` writes to the agent's home directory, and
`--dir` writes to a path you name, for a Dockerfile or a CI job. `update`
rewrites only files that differ from what's published, including files you
edited. The full list of agents and flags is in
[docs/commands.md](./docs/commands.md#agent-skills).

### Generate skills

```sh
mapbox generate-skills
```

Writes this CLI's whole command surface as an [Agent
Skill](https://code.claude.com/docs/en/skills), into the current project for
each agent it finds: `.claude/skills` for Claude Code, `.agents/skills` for
Codex. `--agent`, `--global`, `--dir` and `--service` narrow it, and
`--dry-run` lists the files first. To remove every copy it wrote:

```sh
mapbox agent-skills uninstall mapbox-cli
```

### MCP servers

```sh
mapbox mcp list
mapbox mcp install
```

Different kind of "install" from `agent-skills`/`generate-skills` above:
those write a directory this CLI owns, this registers an MCP server —
direct tool-calling access to Mapbox's APIs, not just guidance about them —
with a coding agent's *own* config, since that config belongs to the agent
and may already list other servers. Claude Code, Codex, VS Code and Cursor
are supported today, against the hosted Mapbox MCP endpoints: no token, no
npm package, no Node version to manage. An existing server with the same
name is left alone rather than replaced. VS Code and Cursor currently
register for every project regardless of `--global`, since neither has a
working way to scope it to one; Codex may report a server as installed with
its own login incomplete, an OAuth incompatibility between Codex and
Mapbox's hosted MCP server rather than something this command controls.

## Global options

These apply to every command, not just the API ones.

### Dry runs

Every mutating command takes `--dry-run`: it prints the request instead of
sending it, checking `--data`/`--file` along the way.

```sh
$ mapbox styles delete zz-clitest-style --dry-run
Dry run — nothing was sent.
DELETE https://api.mapbox.com/styles/v1/you/zz-clitest-style?access_token=<redacted>
```

Goes after the operation name (`mapbox styles delete ID --dry-run`),
not before. A read-only command rejects it.

### Timeouts

| Request | Budget |
| --- | --- |
| A normal request (`GET`, or a typed `--data` body) | 60 seconds |
| A `--file` upload, or a `--data @<path>` / `@-` body | 15 minutes |

`--timeout <SECONDS>` overrides either, `MAPBOX_TIMEOUT` sets it for a
whole shell.

### Extra query parameters

`MAPBOX_CLI_EXTRA_QUERY` appends raw query parameters to every request this
process sends, in the same `k1=v1&k2=v2` shape as a URL's own query string —
for an API parameter this CLI's specs don't declare a flag for. `--debug`
and `--dry-run` show it alongside everything else on the request.

### Proxies

`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` are all honored, so
a CLI behind a corporate proxy needs no configuration of its own.

Two things are easy to lose an afternoon to:

- **`HTTP_PROXY` alone does not carry Mapbox requests.** Every Mapbox base
  URL is `https`, and that variable applies to `http` URLs only. Set
  `HTTPS_PROXY` (or `ALL_PROXY`) instead.
- **A SOCKS proxy is not supported.** `ALL_PROXY=socks5://…` fails the request
  rather than being ignored, and the failure says `unsupported scheme
  socks5`. If you see that, it is the proxy and not the network.

### Output format

`--output`/`-o` (or `MAPBOX_OUTPUT`) picks the shape:

| Value | Result |
| --- | --- |
| `auto` (default) | `text` at a terminal, `json` when piped |
| `text` | Pretty-printed, readable |
| `json` | Everything on stdout is JSON: one compact document per command |

Every command prints one JSON document today. A command that streams would
print one per line (JSON Lines), which is why there is no `-o jsonl`.

Errors always go to stderr and never appear in stdout. Under `json` they're
one flat object: `code`, `message`, plus `fix`, `next_actions` and `docs`
where there is advice to give. See [docs/commands.md](./docs/commands.md#errors)
for the list of codes.

At a terminal, a run opens with a `mapbox · v<version>` line on stderr, and
tables, labels and tips are in color. Neither reaches a pipe or a file.
`-q`/`--quiet` (or `MAPBOX_QUIET=1`) hides the banner; `NO_COLOR` turns
color off.

### `--schema`

`mapbox <command> --schema` describes a command as JSON instead of running
it: arguments, types, and the request it would make. Needs no token.

```sh
mapbox styles get --schema         # one command
mapbox --schema                     # the whole CLI
```

### Confirmation and `--yes`

Only `DELETE` commands ask for confirmation, and only at a terminal (both
stdin and stderr):

```console
$ mapbox styles delete my-style
About to DELETE https://api.mapbox.com/styles/v1/me/my-style
Continue? [y/N] n
```

`--yes`/`-y`/`MAPBOX_YES=1` skips the question, which is what CI wants, or a
script deliberately run at a terminal. It does **not** apply to `auth login`,
which always needs a person.

## Updates, history and logs

### Update notices

`mapbox` can't update itself, so when a newer release exists it says so once
a day, on stderr, at a terminal:

```
A newer mapbox is available: 0.2.0 (this is 0.1.5).
Update: curl -fsSL https://cli.mapbox.com/install.sh | sh
Silence this: MAPBOX_NO_UPDATE_CHECK=1
```

The suggested command is the install script's. If you installed with
Homebrew, run `brew upgrade mapbox` instead.

This is the only request the CLI makes that you didn't ask for, so it is
kept narrow:

| | |
| --- | --- |
| What it sends | A `GET` for the channel's `latest/manifest.json`, with no token, no account, no command, and nothing about you or your machine beyond `User-Agent: mapbox-cli/<version>` |
| When | At most once a day, and only when stderr is a terminal, so CI and piped runs never check and never print |
| Where | A detached background process. Your command never waits on it: offline, the timing is unchanged and nothing is printed |
| Off | `MAPBOX_NO_UPDATE_CHECK=1`, or `MAPBOX_CLI_NO_TELEMETRY=1`, which silences this too, for the shell session it's set in |

`~/.mapbox/update-check.json` (or `$MAPBOX_CONFIG_DIR`) holds the answer
between runs. A build that names no release channel never checks at all, and
`cargo build` produces one.

`mapbox config set update-check off` turns it off in every shell, not just
the one an environment variable is set in. See
[Config](./docs/commands.md#config).

### Command history

Each run appends one line to `~/.mapbox/history/<UTC date>.jsonl` (or under
`$MAPBOX_CONFIG_DIR`), kept for 30 days and at most 10 MB, oldest dropped
first: which command ran (its command path, like `search forward`), how it
ended, how long it took and the request ids support can look up. Argument values are never recorded — not what you
searched for, not a file path, not a token. The files are readable only by
you and never leave your machine.

```sh
mapbox history list             # the most recent runs, newest first
mapbox history show             # everything recorded about the newest run
mapbox history show be40d711    # or one run, by any prefix of its id
```

`--help`, `--version`, `completion`, `history` itself and runs under `sudo`
are not recorded. `mapbox config set history off` turns history off for
good, and `MAPBOX_HISTORY=0` for one shell; with it off, nothing is written
and no directory is created, but what was already recorded stays until you
delete `~/.mapbox/history`. `MAPBOX_CLI_NO_TELEMETRY` does not affect it.

### Diagnostic logs

Off by default. `mapbox config set log on` (or `MAPBOX_LOG=1` for one shell)
adds, for each run history records, a line of detail in
`~/.mapbox/logs/<UTC date>.jsonl`: the command line, each request's method,
URL, status, request id and timing, which token was used (where it came
from, its type and account, never the token itself) and the error message.
Tokens are replaced with `<redacted>` wherever they appear, and the files
never leave your machine.

`mapbox history show` includes a run's log, or says it was not captured
(logging was off) or is no longer available. Logs are kept up to 30 days and
100 MB in total; past that the oldest go first, and the run's history record
stays. A day of logs goes when that day of history does. Logging needs
history: with history off it never runs, and with the `history` setting off
`config set log on` refuses.

## Privacy

**YOUR PRIVACY - COLLECTION OF TELEMETRY**

Mapbox collects telemetry data from our CLIs to better understand how our
tools are used and how to improve our products.

- **What Telemetry Data We Collect:** Usage metrics include installs, the
  service a Mapbox API command belongs to (e.g. `styles` or `geocoder`,
  never the operation or its arguments), CLI version, OS/architecture,
  whether stdin and stdout are attached to a terminal, an identifier for
  the detected AI coding agent (if any) running the command (based on
  signals such as the presence of the `CLAUDECODE` or `COPILOT_MODEL`
  environment variable; see [`agent_detect.rs`](./src/agent_detect.rs) for
  the complete, versioned list), and a boolean flag indicating whether the
  command was run in a CI environment. IP addresses necessarily accompany
  any request made to our server, but will not be retained and analyzed
  together with telemetry data.
- **Why We Collect It:** For internal analytics by Mapbox to understand
  adoption, prioritize investments, and improve the reliability,
  performance, and developer experience of our CLIs.
- **What We Do Not Collect:** Code completion outputs, source code, project
  file names, directory contents, non-Mapbox API keys, or credentials.
- **Who has Access:** Telemetry data will not be disclosed to, or accessed
  by, third parties other than Mapbox affiliates and passive cloud storage
  and hosting providers necessary to maintain our infrastructure.

This also covers [update notices](#update-notices) above, since that check
rides the same opt-out.

**How to Opt Out:** The collection of telemetry data is enabled by default.
You can disable it at any time and without affecting the functionality of
our CLIs by setting

```sh
MAPBOX_CLI_NO_TELEMETRY=1
```

For additional information on our data processing activities and your
related rights, please see our Mapbox
[Privacy Policy](https://www.mapbox.com/legal/privacy).

## Contributing

Running the tests, the lints they have to pass, versioning rules and how
specs become commands are in [CONTRIBUTING.md](./CONTRIBUTING.md).
