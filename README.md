# mapbox-cli

A command-line interface for Mapbox APIs. Commands are generated at build
time from OpenAPI specs, so they always match the specs.

## Contents

- [Build from source](#build-from-source)
- [Install a released binary](#install-a-released-binary)
  - [Download the archive yourself](#download-the-archive-yourself)
- [Commands](#commands)
  - [Auth](#auth)
    - [Named profiles](#named-profiles)
  - [API Related](#api-related)
  - [Agent skills](#agent-skills)
  - [Shell completion](#shell-completion)
  - [Generate Skills](#generate-skills)
  - [Tileset CLI](#tileset-cli)
- [Usage](#usage)
  - [Dry runs](#dry-runs)
  - [Timeouts](#timeouts)
  - [Output format](#output-format)
  - [`--schema`](#--schema)
  - [Confirmation and `--yes`](#confirmation-and---yes)
  - [Update notices](#update-notices)
 - [Privacy](#privacy)
- [Uninstall](#uninstall)
- [Contributing](#contributing)

## Build from source

This is the buildable core, and building it is the primary way to get it
from here. It needs [Rust via rustup](https://rustup.rs) and nothing else:
no second repository, no token, and no network beyond crates.io.

```sh
cargo build --release
./target/release/mapbox --help
```

The OpenAPI specs the commands are generated from are vendored in
`openapi/`, so a clone compiles on its own.

## Install a released binary

Mapbox publishes builds for macOS, Linux and Windows. The install script
detects your platform, checks a SHA-256 checksum, and installs `mapbox`.
No `sudo`, no admin rights:

```sh
curl -fsSL https://cli.mapbox.com/install.sh | sh
```

```powershell
irm https://cli.mapbox.com/install.ps1 | iex
```

`MAPBOX_CLI_VERSION` pins a version instead of taking the newest:

```sh
curl -fsSL https://cli.mapbox.com/install.sh | MAPBOX_CLI_VERSION=0.2.1 sh
```

```powershell
$env:MAPBOX_CLI_VERSION = '0.2.1'; irm https://cli.mapbox.com/install.ps1 | iex
```

With or without the leading `v`: `0.2.1` and `v0.2.1` both work, so the
version `mapbox --version` prints can be pasted straight in.
`MAPBOX_INSTALL_DIR` chooses where the binary lands, and defaults to
`~/.local/bin`.

`scripts/install.sh` and `scripts/install.ps1` here are those installers'
sources; `scripts/test-install.sh` and `scripts/test-install.ps1` exercise
them end to end without touching the network.

Run `mapbox --help` once it's on your `PATH`.

### Download the archive yourself

Nothing about the install script is required. If piping one into a shell is
not allowed where you work, the archives are ordinary HTTP downloads, and
`manifest.json` lists every target with its checksum:

```sh
curl -fsSL https://cli.mapbox.com/latest/manifest.json
```

Six targets are published: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`,
`x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`. Pick yours, check it,
then extract:

```sh
version=v0.2.1
file=mapbox-${version}-aarch64-apple-darwin.tar.gz

curl -fsSLO "https://cli.mapbox.com/${version}/${file}"
curl -fsSL "https://cli.mapbox.com/${version}/SHA256SUMS" |
    grep "$file" | shasum -a 256 -c -

tar -xzf "$file"
mv mapbox ~/.local/bin/
```

On Windows the archive is a `.zip` and PowerShell can read the manifest
directly, so there is no text file to parse:

```powershell
$version = 'v0.2.1'
$target  = 'x86_64-pc-windows-msvc'

$manifest = Invoke-RestMethod "https://cli.mapbox.com/$version/manifest.json"
$artifact = $manifest.artifacts.$target

Invoke-WebRequest "https://cli.mapbox.com/$version/$($artifact.file)" -OutFile $artifact.file
if ((Get-FileHash -Algorithm SHA256 $artifact.file).Hash -ine $artifact.sha256) {
    throw 'checksum mismatch'
}

Expand-Archive $artifact.file -DestinationPath .
```

`Invoke-WebRequest` and `Get-FileHash` rather than `curl` and `sha256sum`:
the first is an alias for something else in Windows PowerShell and neither of
the others is guaranteed to be present.

Use `latest` in place of the version for whatever is current. Each archive
holds one file, the `mapbox` executable, so there is no directory to step
into and nothing else to place. `~/.local/bin` is where the install script
puts it too, and `MAPBOX_INSTALL_DIR` is the variable it reads if you prefer
somewhere else.

The macOS builds are not code-signed with a Developer ID. `curl` attaches no
quarantine flag, which is why the commands above run, but a download through
a browser does, and Gatekeeper will refuse an unsigned binary that carries
one. Clear it with `xattr -d com.apple.quarantine mapbox`.

## Commands

### Auth

```sh
mapbox auth login     # opens a browser (OAuth/PKCE)
mapbox auth logout    # removes stored credentials
mapbox auth refresh   # force-refreshes the access token
mapbox auth whoami    # reports which token the next command will use
mapbox auth profiles  # lists every stored profile, not just one
```

Credentials live in `~/.mapbox` as plain JSON with locked-down file
permissions. There is no OS keychain integration. Override them with
`--token`/`--username` or `MAPBOX_ACCESS_TOKEN`/`MAPBOX_USERNAME`, and set
`MAPBOX_CONFIG_DIR` to move the whole store, which a container usually
wants.

#### Named profiles

`--profile <name>` keeps a separate credential set per account:

```sh
mapbox auth login --profile android_app
mapbox --profile android_app styles list
mapbox auth profiles              # which profiles are actually stored
```

### API Related

Each API is a top-level subcommand, one sub-subcommand per operation:

```sh
mapbox accounts *
mapbox fonts *
mapbox geocoder *
mapbox rasterarrays *
mapbox search *
mapbox sprites *
mapbox static-images *
mapbox static-tiles *
mapbox styles *
mapbox tilequery *
mapbox tilesets *
```

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

[docs/commands.md](./docs/commands.md) lists every command.

Every request sends `User-Agent: mapbox-cli/<version>` and nothing else
about you or your machine. `MAPBOX_CLI_NO_TELEMETRY=1` keeps even future markers
out of that header.

Each run also appends one event to `~/.mapbox/.telemetry/<date>.jsonl`
(kept for 7 days): the command's name, its options (a value only when it
comes from a fixed list, otherwise just its length or size), how it ended,
and how long it took — never a token, a file path or free text you typed.
It stays on this machine. `MAPBOX_CLI_NO_TELEMETRY=1` or
`mapbox config set telemetry off` turns it off.

### Agent skills

```sh
mapbox agent-skills list        # what's published, and what's installed here
mapbox agent-skills install     # all 20, into whichever agents you have
mapbox agent-skills update      # re-install what's here, report what changed
mapbox agent-skills uninstall <NAME>
```

Installs the [Mapbox Agent Skills](https://github.com/mapbox/mapbox-agent-skills):
hand-written guidance for coding agents on cartography, token security, style
quality, geospatial operations and the mobile and web SDKs. No token needed,
and no Node. It's one tarball, extracted in the binary.

Fifteen agents are supported: Claude Code, Codex, Cursor, Cline, Gemini CLI,
GitHub Copilot, Zed, OpenCode, Amp, Windsurf, Roo Code, Continue, Kiro CLI,
Qwen Code and Goose. By default, skills are installed for whichever of them
are found on the machine.

| Flag | What it does |
| --- | --- |
| `--agent <name>` | Install for one agent. Repeatable. |
| `--global` | Write to the agent's home directory instead of this project. |
| `--dir <path>` | Write to a directory you name, for a Dockerfile or a CI job. |

Most of these agents read the same `.agents/skills` directory, so asking for
several usually means a single write.

`--ref <branch|tag|sha>` installs a particular version and a SHA pins it;
`--dry-run` lists the files first. A skill directory that already exists stops
the install until `--force`, since it may hold your edits.

`update` compares what's installed with what's published, byte for byte, and
rewrites only what differs, including restoring a file you edited. It never
installs a skill that wasn't already there. `uninstall <NAME>` removes the
directory, asking first at a terminal; it makes no network request at all.
There's no lock file: one tarball arrives before any record could be
consulted, so comparing bytes answers exactly and leaves no state to keep in
step with another tool's.

Different command from [`generate-skills`](#generate-skills) below, which
writes a skill describing *this CLI*. These are about using Mapbox.

### Shell completion

```sh
mapbox completion bash | zsh | fish | powershell
```

Prints a completion script on stdout. Nothing is written to disk, so put it
where your shell looks:

```sh
mapbox completion bash > ~/.local/share/bash-completion/completions/mapbox
mapbox completion zsh  > ~/.zfunc/_mapbox            # a directory on $fpath
mapbox completion fish > ~/.config/fish/completions/mapbox.fish
source <(mapbox completion bash)                     # this shell only
```

```powershell
mapbox completion powershell >> $PROFILE
```

It completes commands, subcommands and flag names, generated from this
binary's own command tree. So it matches the build that printed it, and
nothing about it is maintained by hand. Values (style ids, usernames) are not
completed: that would mean an API request mid-keystroke. `--output` does not
apply, because the script is the result.

### Generate Skills

```sh
mapbox generate-skills
```

Writes the whole command surface as an [Agent
Skill](https://code.claude.com/docs/en/skills): `.claude/skills` for
Claude Code, `.agents/skills` for Codex. `--agent`, `--global`, `--dir`,
and `--service` narrow it; `--dry-run` lists files without writing them.

Without `--global` it writes into the current project, once per agent it
finds, and it prints every directory it used. To take them out again:

```sh
mapbox agent-skills uninstall mapbox-cli
```

That removes every copy this command wrote, which is more than deleting the
directories by hand usually catches — a default run writes for each agent on
the machine, not just the one you had in mind.

### Tileset CLI

```sh
mapbox tilesets-cli list <USERNAME>
mapbox tilesets-cli upload-source <USERNAME> <SOURCE_ID> data.geojson.ld
```

Forwards everything to the separately-installed [Tilesets
CLI](https://github.com/mapbox/tilesets-cli):

```sh
pipx install mapbox-tilesets       # Python 3.10+
```

`--output` and `--yes` don't apply here. `tilesets` has its own flags, so use
`--force`/`-f` for its prompts. It needs a token too: `mapbox auth login`
covers it, or pass `--token`/`MAPBOX_ACCESS_TOKEN` the same way as every
other command. `mapbox auth whoami` shows which one is in play if a
tileset command answers for the wrong account.

## Usage

These apply globally, across every command, not just the API ones above.

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

`json` promises the shape, not the count. Every command today returns one
document. A command that streams would emit one per line (JSON Lines), but
that is a property of the command rather than of the flag, so there is no
`-o jsonl`. Nothing streams yet.

Errors always go to stderr and never appear in stdout. Under `json` they're
one flat object: `code`, `message`, plus `fix`, `next_actions` and `docs`
where there is advice to give. See [docs/commands.md](./docs/commands.md#errors)
for the list of codes.

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
script deliberately run at a terminal. It does **not** apply to `auth login`, which always
needs a person.

### Update notices

`mapbox` can't update itself, so when a newer release exists it says so once
a day, on stderr, at a terminal:

```
A newer mapbox is available: 0.2.0 (this is 0.1.5).
Update: curl -fsSL https://cli.mapbox.com/install.sh | sh
Silence this: MAPBOX_NO_UPDATE_CHECK=1
```

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

`mapbox config set update-check off` turns it off for good, in every shell —
see [Config](docs/commands.md#config) — rather than just the session an
environment variable happens to be set in.

### Privacy

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

## Uninstall

```sh
mapbox uninstall
```

Removes only the `mapbox` binary. Credentials, profiles, and the separate
`tilesets` binary are untouched. Run
[`mapbox auth logout`](./docs/commands.md#mapbox-auth-logout) first if you
want those gone too. Asks for confirmation like any destructive command
(`--yes`/`MAPBOX_YES` skips it); `--dry-run` previews without deleting.

## Contributing

Running the tests, the lints they have to pass, versioning rules and how
specs become commands are in [CONTRIBUTING.md](./CONTRIBUTING.md).
