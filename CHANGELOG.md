# Changelog

What changed, and what it means for scripts that already use this tool.
Newest first, written by hand — the commit subject rarely explains why a
change matters.

Versions follow [semantic versioning](https://semver.org/). Pre-1.0 rule:
while the version starts with `0.`, a breaking change raises the minor
number (`0.1.0` → `0.2.0`); everything else raises the patch. What counts as
breaking is [written down in CONTRIBUTING.md](CONTRIBUTING.md#compatibility) — briefly,
command names, flags, the two output modes and the exit codes are promises;
the Mapbox APIs' own response bodies are not.

Cutting a release adds a `## <version> - <date>` heading below
`## Unreleased`, which stays in place so the next change has somewhere to go.

Dev-channel builds (`v0.1.3-dev.<sha>`) are published straight from a branch
that may never merge. They are not releases and are not listed here.

## Unreleased

- `mapbox map-matching`, snapping a noisy GPS trace to the road network and
  returning the route it most likely followed, for driving (with or
  without live traffic), walking, or cycling. No subcommand: like `mapbox
  directions` below, this API has one operation, so there's nothing a
  second word (the old `match`) would disambiguate; see
  `spec::FLATTENED_SERVICES`. Hand-authored into `custom-openapi/` for the
  same reason `mapbox directions` and `mapbox isochrone` were: no upstream
  spec exists yet. Reuses the `profile`-vs-`--profile` collision fix
  (`ARG_NAME_OVERRIDES` gets a third row) and the free-form (not `enum`)
  routing profile, for the same OEM-account reason. Excludes POST, for the
  same reason `directions` does: the API's own POST is for a trace too long
  for a URL, a real gap rather than a design choice.

- `mapbox isochrone`, how far you can get from a point in a given time or
  distance, for driving (with or without live traffic), walking, or
  cycling, returned as GeoJSON polygons or linestrings. No subcommand:
  like `mapbox directions` below, this API has one operation, so there's
  nothing a second word would disambiguate; see `spec::FLATTENED_SERVICES`.
  Hand-authored into `custom-openapi/` for the same reason `mapbox
  directions` was: no upstream spec exists yet. Reuses `directions`'s fix
  for a spec parameter named `profile` colliding with the global
  `--profile` flag (`ARG_NAME_OVERRIDES` already covered the mechanism,
  this is just a second row, not a second fix) and its free-form (not
  `enum`) routing profile, for the same OEM-account reason.

- `mapbox directions`, routes between 2-25 waypoints for driving (with
  or without live traffic), walking, or cycling. No subcommand: this API
  has one operation, so, like `mapbox usage`, there's nothing a second
  word would disambiguate; see `spec::FLATTENED_SERVICES`. Hand-authored
  into `custom-openapi/` rather than waiting on an upstream spec. The
  whole Navigation API category had no CLI coverage before this; excludes
  the ~30 electric-vehicle-routing parameters (`engine=electric` and
  everything under it), which describe one vehicle's charge/discharge
  curve down to the watt and are a poor fit for a hand-typed CLI flag.
  Left for a follow-up.

  The routing profile (`mapbox/driving` etc.) is a free-form value, not a
  fixed set of four: an early version rejected anything else client-side,
  which would have broken this command for exactly the accounts that most
  need it, since some (OEM agreements, mainly) have additional profiles of
  their own never published to docs.mapbox.com. Reported in review before
  this shipped anywhere. Two path-parameter bugs surfaced while wiring the
  original four up and are fixed for every command, not just this one: a
  spec parameter literally named `profile` (the routing profile) silently
  collided with the global `--profile` credentials flag, since clap has one
  namespace of ids per command and a positional of the same name replaced it
  outright; and a path parameter whose every legitimate value contains a
  literal `/` (`mapbox/driving`) was being percent-encoded to `%2F` by the
  same escaping that stops a free-text value from smuggling in extra path
  segments. That's safe to skip for a parameter named in a small table
  (`UNESCAPED_PATH_PARAMS`) whose values are trusted to carry that
  character on purpose.

- `MAPBOX_CLI_EXTRA_QUERY` appends raw query parameters to every request, in
  the same `k1=v1&k2=v2` shape as a URL's own query string — for an API
  parameter this CLI's specs don't declare a flag for.

- A native `aarch64-pc-windows-msvc` build. Windows on Arm ran the x64 build
  under emulation before this — including inside a VM on Apple Silicon, the
  larger of the two populations this serves — which `install.ps1` already
  said out loud and now no longer has reason to. `install.ps1` already asked
  the manifest for this target before falling back to the x64 one, and
  `.cargo/config.toml` already named it, so publishing the artifact was the
  whole client-side change.

- `install.sh`/`install.ps1` now document a convention for `MAPBOX_CLI_INSTALL_SOURCE`
  when a coding agent invokes the installer on someone's behalf: `agent-<name>`
  (`agent-claude-code`, `agent-cursor`), so an access-log query can tell those
  installs apart from a human or CI one. A dash rather than the slash an
  earlier proposal used — `/` is stripped by the installers' own sanitizer,
  which would have collapsed `agent/claude-code` into `agentclaude-code` and
  lost the separator. Comment-only: neither installer's behavior changed.

## 0.3.0 - 2026-09-22

### Added

- `mapbox config` — `get`/`set` for settings that persist across shells and
  sessions, written to `~/.mapbox/config.json` (or `$MAPBOX_CONFIG_DIR`)
  rather than an environment variable that only lasts for the session it was
  set in. One setting today: `update-check`, which `mapbox config set
  update-check off` turns off for good, mirroring `MAPBOX_NO_UPDATE_CHECK`.

- `mapbox config list`/`unset`, alongside `get`/`set`. `list` reports every
  setting in one call rather than one key at a time; `unset` clears a
  setting back to "never set" rather than writing its current default value
  explicitly — the difference that lets a later default change reach a
  cleared key but not one a caller pinned to the old default on purpose.

- The README now documents installing without the install script: the
  archives are plain HTTP downloads, `manifest.json` lists every target with
  its checksum, and the commands to verify and extract one are written out.
  Nothing new is published — this is the same channel the install script
  reads, for anyone whose employer does not allow piping a script into a
  shell.

### Changed

- The skill `mapbox generate-skills` writes now tells an agent which of the
  three ways to authenticate it can actually use. It listed all three without
  comment, so an agent with no token would read "credentials stored by
  `mapbox auth login`" as an option and try it — a command that opens a
  browser and waits for a person, which in a coding-agent session can only
  fail. It now says to have `MAPBOX_ACCESS_TOKEN` set, and to ask for one
  rather than reaching for `auth login`. Reported from a real session that hit
  exactly this.

- `mapbox generate-skills` now prints the command that removes what it wrote,
  in both output modes, and the JSON carries it as `remove_with`. The write is
  `generate-skills` and the undo is `agent-skills uninstall`, a differently
  named command belonging to a different feature, so there was no way to get
  from one to the other — a reader of `generate-skills --help` saw no way back.
  Reported after an agent reached for `rm -rf` instead, had it refused by its
  sandbox, and left untracked directories in a git working tree. A dry run does
  not print it, since nothing was written. `--help`, the generated reference
  page and the README say it too.

- Advice that is a command to paste now names every shell instead of guessing
  one. The repair for a file blocking the credential directory offered `mv` and
  `export NAME="$(cat …)"`, which a Windows reader cannot run; it now gives
  `mv`/`Move-Item`/`move` and all four of `export`, fish's `set -gx`,
  PowerShell's `$env: … Get-Content` and `cmd.exe`'s `set /p`, each labeled
  with the shell it belongs to. The tip printed after a successful login no
  longer offers `export MAPBOX_USERNAME=…` either; it says what to set rather
  than how.

  Keying this on the operating system would have been wrong in both
  directions, which is why it does not: PowerShell runs on macOS and Linux and
  Git Bash runs on Windows. Each row also uses the name its shell owns rather
  than one that may be aliased — `Move-Item` and `Get-Content` rather than `mv`
  and `cat`, which resolve differently depending on what is installed.

- The refusal from `mapbox auth login` with no terminal now names both ways
  out. It said "Set MAPBOX_ACCESS_TOKEN for a script or a CI job", which
  describes automation and assumes that is who is asking; a person working
  through a coding agent hits this too, and hits it again when they try the
  command themselves in that agent's shell, which has no terminal either. The
  fix now leads with running it in a terminal window and keeps the token as the
  answer for a script, a CI job or an agent.

- A file sitting where the credential directory belongs now says so when it
  looks like an access token, which the one older Mapbox tools left at
  `~/.mapbox` does. The advice was to move it aside, with nothing to tell the
  reader whether they were moving junk or a working credential; it now says
  the token still works and gives the `MAPBOX_ACCESS_TOKEN` line that keeps
  using it. The token itself is never printed, and a test holds that.

- Code & Command Clean up.

### Fixed

- The README described the published builds as signed. They are not
  code-signed with a Developer ID or an Authenticode certificate; what the
  install script checks is a SHA-256 checksum. The claim is gone, and the new
  section says what a macOS user hits because of it: Gatekeeper refuses an
  unsigned binary carrying a quarantine flag, which a browser download sets
  and `curl` does not.

- The URL printed by `--debug`, and by a text-mode `--dry-run`, is now a URL.
  Query values were concatenated raw, so any value containing a space rendered
  with literal spaces and `curl` refused it outright — which is the one thing
  that line is printed for. Found with `mapbox search forward --q "Dog
  friendly coffee shops near me"`, an ordinary call now that the Search Box
  API takes free text. The request itself was never affected: reqwest encodes
  what it sends, and a JSON dry run keeps `url` and `query` as separate
  fields, so only this rendering was wrong. Values are percent-encoded with
  `%20` rather than form-urlencoding's `+`, and `,` `:` `/` `@` are kept
  literal, so a coordinate or a style URI still reads as one.

  It also stops a value from misrepresenting the request. A value holding `&`
  used to split into another `name=value` pair, so the line claimed a
  parameter the request never carried, and anyone pasting it sent something
  different from what was being debugged.

- `MAPBOX_CLI_VERSION` now accepts a version with or without the leading `v`.
  The channel's directories are named `v0.2.1`, but every place a person reads
  a version from shows it without one — `mapbox --version`, this file,
  `Cargo.toml` — so the spelling somebody would copy was the one that failed,
  and it failed as a bare `403` from S3 on a path that does not exist. That
  reads as "you are not allowed" rather than "no such version". `latest` and
  any other non-numeric channel name are untouched. Both installers, both
  covered by their suites.

- `MAPBOX_CLI_VERSION` and `MAPBOX_INSTALL_DIR` are documented in the README,
  which never mentioned either of them.

## 0.2.2 - 2026-09-15

### Changed

- `mapbox usage` is no longer described as a private preview: the Statistics
  API it calls is generally available, so `mapbox auth login` now requests
  `statistics:read` unconditionally instead of through a feature flag.
  Nothing about who can run the command or what it prints changed — the flag
  it used to go through was already on for everyone.

- The 403 a missing `statistics:read` scope gets back now leads with "run
  `mapbox auth login` again", the fix that actually applies, before falling
  back to "contact Mapbox support" for the rarer case of an account with no
  access at all. It used to jump straight to support, which was written for
  the old private-preview gate and never distinguished the two — the API
  answers both with 403, not the 401/403 split the docs previously assumed.

## 0.2.1 - 2026-09-15

### Fixed

- A path parameter can no longer change the shape of the request URL. Values
  are substituted into a path template, so one carrying URL syntax altered
  where the request went rather than naming a segment in it: `?` appended
  query parameters the caller never asked for, `..` (and its backslash
  spelling) moved the path, and `#` truncated it — each with the caller's
  token and the command's method attached. The host was never reachable, so
  nothing could be directed at another server. `/`, `?`, `#` and `\` are now
  percent-encoded, and a value of `.` or `..` is refused as
  `invalid_path_parameter`. Punctuation these values legitimately carry — a
  static-images overlay, `@2x`, `.png`, a comma-separated coordinate — is
  untouched. See [#15](https://github.com/mapbox/mapbox-cli/pull/15).

## 0.2.0 - 2026-09-14

### Added

- Paginated listings now say when there is more to fetch. A response the API
  paged answers with one page, and the CLI prints the flags that fetch the
  next one — "More results: add `--limit 2 --start …` for the next page" —
  on stderr in both output modes. Before this the extra pages were
  unreachable: the API signals them in a `Link` header, which nothing read,
  so `-o text` and `-o json` both looked complete. `--id` on a paged listing
  now also distinguishes "not on this page" from "does not exist", and says
  how to look further. Following the pages is still the caller's job; there
  is no `--all` yet.

- Failures now carry the response's request id, which is what Mapbox support
  needs to find one request in their logs. In practice that is CloudFront's
  `x-amz-cf-id`, which every Mapbox response carries; a service sending its
  own `x-request-id` is preferred when one does. Present on every failure
  under `-o json` as `request_id`; printed under `-o text` for a 5xx only,
  where the server is at fault and there is nothing the caller can do about
  it. `mapbox agent-skills` is exempt — it fetches from GitHub, whose request
  id Mapbox support cannot look up.

- `--data` can now read the request body instead of carrying it: `@<path>`
  reads a file and `@-` reads stdin, the spelling curl uses. Before this the
  only way to send a style from a file was `--data "$(cat style.json)"`,
  which has no `cmd.exe` equivalent on a platform this CLI ships installers
  for, put the body in the process table where `ps` shows it, and broke at a
  size that failed in the shell before `mapbox` ran — so no error message
  could explain it. Five operations take `--data`. A body read this way also
  gets the 900-second transfer budget rather than the 60-second one, since
  nothing bounds a file the way a command line bounds what can be typed.

### Changed

- **Breaking**: nine more commands renamed, continuing #116's cleanup, and
  two dropped outright:

  | Was | Is now |
  | --- | --- |
  | `mapbox geocoder forward-geocode` | `mapbox geocoder forward` |
  | `mapbox geocoder reverse-geocode` | `mapbox geocoder reverse` |
  | `mapbox geocoder batch-geocode` | `mapbox geocoder batch` |
  | `mapbox tilesets get-rastertile` | `mapbox tilesets get-tile` |
  | `mapbox tilesets get-vectortile` | `mapbox tilesets get-mvt` |
  | `mapbox rasterarrays get-mrt-tile` | `mapbox tilesets get-mrt` |
  | `mapbox tilequery get` | `mapbox tilesets query` |
  | `mapbox static-images get-static-image` | `mapbox static get-image` |
  | `mapbox static-tiles get-static-tile` | `mapbox static get-tile` |

  `mapbox rasterarrays`, `mapbox tilequery`, `mapbox static-images` and
  `mapbox static-tiles` no longer exist: each held exactly one operation,
  and that operation now answers under `tilesets` or `static` instead —
  the same reasoning 0.1.8 gave for `sprites` and `tilesets` appearing
  there. `mapbox static` is new for it.

  `static-images get-static-image-auto` and `get-static-image-bbox` are
  gone, not renamed — the decision record's reason for withholding both is
  that they will merge into `get-image`'s own parameters, but that merge
  hasn't happened yet, so today there is simply no way to ask for an
  auto-fit or bounding-box static image from this CLI.

  Nothing answers to any of the old spellings, the same as 0.1.8's rename:
  no hidden alias, and the two dropped commands are not offered under any
  spelling.

- The advice under a transport failure now names `ALL_PROXY` alongside
  `HTTPS_PROXY` and `NO_PROXY`, and says that a SOCKS proxy is not supported.
  `ALL_PROXY=socks5://…` fails the request rather than being ignored, and
  `unsupported scheme socks5` in the message is the part that distinguishes
  it from the network being down.

- `rand` moved from 0.8 to 0.10. No behavior changes: the two places it is
  used — the PKCE verifier and the OAuth `state` in `mapbox auth login` —
  draw from `ThreadRng` before and after, which `rand` declares a CSPRNG, and
  `thread_rng().gen()` becoming `random()` is a rename. Recorded because it
  is the crate that generates those two values, so a login problem around
  this release should be able to find it. Both are now covered by tests
  against RFC 7636, which they were not before.

- Both installers now honor `MAPBOX_CLI_NO_TELEMETRY`, the name the binary
  reads. They were left on the old `DISABLE_TELEMETRY` when the binary was
  renamed, so neither name silenced both halves: the documented variable
  stopped the CLI's markers but not the installer's, and the old one did the
  reverse. `DISABLE_TELEMETRY` keeps working **in the installers only** — the
  binary's break was announced, and a script fetched and run in one line has
  no release notes in front of the reader, so breaking an opt-out there would
  have happened silently. When both are set the new name wins.

- A usage error under `-o json` now carries clap's own suggestion as `fix`:
  `mapbox styles lst` answers `"fix": "A similar subcommand exists: 'list'"`.
  Clap renders that tip in a paragraph of its own, and `message` is built from
  the first one, so `json` consumers — scripts and agents — were the only ones
  not told what was probably meant. It matters most for the renames above: a
  script pinned to a command that no longer exists now gets a pointer to the
  one that replaced it. `-o text` is unchanged, where clap already printed it.
  Misspelled flags are covered too.

### Security

- `rustls` moved to 0.23.45, fixing
  [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285) —
  "TLS 1.3 handshake messages incorrectly accepted across encryption level
  boundaries", medium severity, published 2026-09-14. `rustls` is reached
  through `reqwest`, so every HTTPS request this CLI makes used the affected
  version; nothing in the crate itself had to change. Fixed in
  [mapbox/mapbox-cli#2](https://github.com/mapbox/mapbox-cli/pull/2).

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
