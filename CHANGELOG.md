# Changelog

What changed, and what it means for scripts that already use this tool.
Newest first.

Versions follow [semantic versioning](https://semver.org/). Pre-1.0 rule:
while the version starts with `0.`, a breaking change raises the minor
number (`0.1.0` → `0.2.0`); everything else raises the patch. What counts as
breaking is [written down in CONTRIBUTING.md](CONTRIBUTING.md#compatibility) — briefly,
command names, flags, the two output modes and the exit codes are promises;
the Mapbox APIs' own response bodies are not.

## Unreleased

### Security

- `rustls` moved to 0.23.45 in `Cargo.lock`, fixing
  [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285) —
  "TLS 1.3 handshake messages incorrectly accepted across encryption level
  boundaries", medium severity, published 2026-09-14. `rustls` is reached
  through `reqwest`, so every HTTPS request this CLI makes used the affected
  version; nothing in this crate had to change. Lockfile only, and the one
  crate.

## 0.1.8 - 2026-09-14

Initial beta release. The next release is `0.2.0`.

### Added

- Commands generated from the Mapbox OpenAPI specs, covering `accounts`,
  `fonts`, `geocoder`, `maps`, `rasterarrays`, `search`, `static-images`,
  `static-tiles`, `styles`, `tilequery` and `vectortiles`.
- `mapbox auth login`, `whoami`, `logout` and `refresh`: a browser OAuth
  flow, credentials stored per `--profile`, and `whoami` answering which
  token the next command would actually use.
- `mapbox tilesets-cli …`, forwarding its arguments verbatim to the Python
  Tilesets CLI with the resolved token injected through the child's
  environment rather than its argv.
- `mapbox usage`, showing account or per-token usage by Mapbox product and
  day.
- `mapbox agent-skills list`, `install`, `update` and `uninstall`, installing
  the [Mapbox Agent Skills](https://github.com/mapbox/mapbox-agent-skills)
  for coding agents. `mapbox generate-skills` writes a skill describing this
  CLI's own commands, for fifteen agents with auto-detection.
- `mapbox completion bash|zsh|fish|powershell`, printing a shell completion
  script for commands, subcommands and flag names.
- `mapbox uninstall`, removing the running binary and nothing else.
- `--schema`: what a command takes and what request it would make, as JSON,
  for one command, one service or the whole surface — no token required.
- `--dry-run` on every mutating command.
- `-o json` and `-o text` (also `MAPBOX_OUTPUT`), with stdout carrying the
  result and nothing else, and progress, warnings and errors on stderr.
- A confirmation before a destructive request, asked only at a terminal,
  and `--yes` to answer it in advance.
- `--timeout <SECONDS>` and `MAPBOX_TIMEOUT`, and a distinct
  `request_timed_out` error naming the flag when a request runs out of it.
- `install.sh` and `install.ps1` for macOS, Linux and Windows.
- A background check for a newer release, at most once a day, at a
  terminal. `MAPBOX_NO_UPDATE_CHECK=1` or `MAPBOX_CLI_NO_TELEMETRY=1` turns
  it off.
- Deprecation notices for a retired command or an endpoint its own OpenAPI
  spec marks deprecated.
- Every request identifies itself with `User-Agent: mapbox-cli/<version>`,
  extended with a few non-identifying markers (OS, architecture, CI
  presence, detected coding-agent, TTY state, command group) that
  `MAPBOX_CLI_NO_TELEMETRY=1` drops entirely. See
  [README.md's Privacy section](README.md#privacy).
