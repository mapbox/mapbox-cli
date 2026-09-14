# mapbox-cli

A command-line interface for Mapbox APIs. Commands are generated at build
time from OpenAPI specs, so they always match the specs.

## Contents

- [Build from source](#build-from-source)
- [Install a released binary](#install-a-released-binary)
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
from here. Needs [Rust via rustup](https://rustup.rs) and nothing else — no
second repository, no token, no network beyond crates.io:

```sh
cargo build --release
./target/release/mapbox --help
```

The OpenAPI specs the commands are generated from are vendored in
`openapi/`, so a clone compiles on its own.

## Install a released binary

Official signed builds for macOS, Linux and Windows are published by Mapbox
from a separate repository, and the install script detects your platform,
checks a SHA-256 checksum, and installs `mapbox` — no `sudo`, no admin
rights:

```sh
curl -fsSL https://cli.mapbox.com/install.sh | sh
```

```powershell
irm https://cli.mapbox.com/install.ps1 | iex
```

That channel is not serving yet. Until it is, build from source above.

`scripts/install.sh` and `scripts/install.ps1` here are those installers'
sources; `scripts/test-install.sh` and `scripts/test-install.ps1` exercise
them end to end without touching the network.

Run `mapbox --help` once it's on your `PATH`.

## Commands

### Auth

```sh
mapbox auth login     # opens a browser (OAuth/PKCE)
mapbox auth logout    # removes stored credentials
mapbox auth refresh   # force-refreshes the access token
mapbox auth whoami    # reports which token the next command will use
```

Credentials live in `~/.mapbox` (plain JSON, file permissions locked down —
no OS keychain). Override with `--token`/`--username` or
`MAPBOX_ACCESS_TOKEN`/`MAPBOX_USERNAME`; `MAPBOX_CONFIG_DIR` moves the whole
store, e.g. for a container.

#### Named profiles

`--profile <name>` keeps a separate credential set per account:

```sh
mapbox auth login --profile android_app
mapbox --profile android_app styles list
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
belongs to is decided per operation, so `sprites` and `tilesets` are each
assembled from operations the Styles, Raster Tiles and Vector Tiles specs
declare. `mapbox tilesets` is also unrelated to `mapbox tilesets-cli`,
which proxies to the separate Python tool.

An operation can nest one level deeper where a group reads better —
`mapbox styles draft get`, `draft update`, `draft delete`.

For example:

```sh
mapbox styles get <STYLE_ID>
mapbox styles create --data '{"name": "My Style", "version": 8, ...}'
```

[docs/commands.md](./docs/commands.md) lists every command.

Every request sends `User-Agent: mapbox-cli/<version>` and nothing else
about you or your machine. `MAPBOX_CLI_NO_TELEMETRY=1` keeps even future markers
out of that header.

### Agent skills

```sh
mapbox agent-skills list        # what's published, and what's installed here
mapbox agent-skills install     # all 20, into whichever agents you have
mapbox agent-skills update      # re-install what's here, report what changed
mapbox agent-skills uninstall <NAME>
```

Installs the [Mapbox Agent Skills](https://github.com/mapbox/mapbox-agent-skills)
— hand-written guidance for coding agents on cartography, token security,
style quality, geospatial operations and the mobile and web SDKs. No token
needed, and no Node: one tarball, extracted in the binary.

Fifteen agents are known — Claude Code, Codex, Cursor, Cline, Gemini CLI,
GitHub Copilot, Zed, OpenCode, Amp, Windsurf, Roo Code, Continue, Kiro CLI,
Qwen Code and Goose — and by default it installs for whichever of them are on
the machine. `--agent <name>` picks one (repeatable), `--global` writes to the
agent's home directory instead of this project, and `--dir <path>` writes
somewhere specific, which is what a Dockerfile or a CI job wants. Most of
those agents read the same `.agents/skills` directory, so asking for several
is usually one write.

`--ref <branch|tag|sha>` installs a particular version and a SHA pins it;
`--dry-run` lists the files first. A skill directory that already exists stops
the install until `--force`, since it may hold your edits.

`update` compares what's installed with what's published, byte for byte, and
rewrites only what differs — including restoring a file you edited. It never
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

Prints a completion script on stdout — nothing is written to disk, so put it
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
binary's own command tree — so it matches the build that printed it and
nothing about it is maintained by hand. Values (style ids, usernames) are not
completed: that would mean an API request mid-keystroke. `--output` does not
apply — the script is the result.

### Generate Skills

```sh
mapbox generate-skills
```

Writes the whole command surface as an [Agent
Skill](https://code.claude.com/docs/en/skills) — `.claude/skills` for
Claude Code, `.agents/skills` for Codex. `--agent`, `--global`, `--dir`,
and `--service` narrow it; `--dry-run` lists files without writing them.

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

`--output` and `--yes` don't apply here — `tilesets` has its own; use
`--force`/`-f` for its prompts. It needs a token too: `mapbox auth login`
covers it, or pass `--token`/`MAPBOX_ACCESS_TOKEN` the same way as every
other command. `mapbox auth whoami` shows which one is in play if a
tileset command answers for the wrong account.

## Usage

These apply globally, across every command — not just the API ones above.

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

### Proxies

The standard variables are honoured — `HTTPS_PROXY`, `HTTP_PROXY`,
`ALL_PROXY` and `NO_PROXY` — so a CLI behind a corporate proxy needs no
configuration of its own.

Two things are worth knowing, because both are easy to spend an afternoon on:

- **`HTTP_PROXY` alone does not carry Mapbox requests.** Every Mapbox base
  URL is `https`, and that variable applies to `http` URLs only. Set
  `HTTPS_PROXY` (or `ALL_PROXY`) instead.
- **A SOCKS proxy is not supported.** `ALL_PROXY=socks5://…` fails the request
  rather than being ignored, and the failure says `unsupported scheme socks5`
  — so if you see that, it is the proxy and not the network.

### Output format

`--output`/`-o` (or `MAPBOX_OUTPUT`) picks the shape:

| Value | Result |
| --- | --- |
| `auto` (default) | `text` at a terminal, `json` when piped |
| `text` | Pretty-printed, readable |
| `json` | Everything on stdout is JSON — one compact document per command |

`json` promises the shape, not the count. Every command today returns one
document; a command that streams would emit one per line (JSON Lines), which
is a property of that command rather than of the flag — there is no
`-o jsonl`. Nothing streams yet.

Errors always go to stderr and never appear in stdout. Under `json` they're
one flat object: `code`, `message`, and — where there's advice —
`fix`/`next_actions`/`docs`. See [docs/commands.md](./docs/commands.md#errors)
for the list of codes.

### `--schema`

`mapbox <command> --schema` describes a command as JSON instead of running
it — arguments, types, and the request it would make. Needs no token.

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

`--yes`/`-y`/`MAPBOX_YES=1` skips the question — for CI, or a script run at
a terminal on purpose. It does **not** apply to `auth login`, which always
needs a person.

### Update notices

`mapbox` can't update itself, so when a newer release exists it says so —
once a day, on stderr, at a terminal:

```
A newer mapbox is available: 0.2.0 (this is 0.1.5).
Update: curl -fsSL https://cli.mapbox.com/install.sh | sh
Silence this: MAPBOX_NO_UPDATE_CHECK=1
```

This is the only request the CLI makes that you didn't ask for, so it is
kept narrow:

| | |
| --- | --- |
| What it sends | A `GET` for the channel's `latest/manifest.json` — no token, no account, no command, nothing about you or your machine beyond `User-Agent: mapbox-cli/<version>` |
| When | At most once a day, and only when stderr is a terminal, so CI and piped runs never check and never print |
| Where | A detached background process. Your command never waits on it: offline, the timing is unchanged and nothing is printed |
| Off | `MAPBOX_NO_UPDATE_CHECK=1`, or `MAPBOX_CLI_NO_TELEMETRY=1`, which silences this too |

`~/.mapbox/update-check.json` (or `$MAPBOX_CONFIG_DIR`) holds the answer
between runs. A build that names no release channel — `cargo build` is one —
never checks at all.

### Privacy

Mapbox collects telemetry from this CLI, on by default, to understand
adoption and improve reliability, performance and developer experience:

| | |
| --- | --- |
| What | Added to the `User-Agent` every request already carries: `os/<os>`, `arch/<arch>`, `env/ci` when running in CI, `agent/<id>` when a known coding-agent environment is detected, whether stdin/stdout are attached to a terminal, and — for a generated API command — `command/<group>` (e.g. `styles`, `geocoder`), never the operation or its arguments. Every request also carries the IP address it's sent from, which is not retained alongside telemetry. |
| What it never includes | AI prompts, code completion output, source code, project or directory names, command arguments, non-Mapbox API keys or credentials. |
| Who sees it | Mapbox only — never disclosed to third parties, aside from the cloud storage and hosting providers that keep the infrastructure running. |
| Off | `MAPBOX_CLI_NO_TELEMETRY=1` — also silences [update notices](#update-notices) above, since that check rides the same opt-out. |

See the [Mapbox Privacy Policy](https://www.mapbox.com/legal/privacy) for
how this fits into data processing generally, and your rights over it.

## Uninstall

```sh
mapbox uninstall
```

Removes only the `mapbox` binary — credentials, profiles, and the separate
`tilesets` binary are untouched. Run
[`mapbox auth logout`](./docs/commands.md#mapbox-auth-logout) first if you
want those gone too. Asks for confirmation like any destructive command
(`--yes`/`MAPBOX_YES` skips it); `--dry-run` previews without deleting.

## Contributing

Running the tests, the lints they have to pass, versioning rules and how
specs become commands are in [CONTRIBUTING.md](./CONTRIBUTING.md).
