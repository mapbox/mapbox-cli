# Contributing to mapbox-cli

Notes for building this CLI from source and working on it. If you just want
to use `mapbox`, see the [README](./README.md) instead.

## Contents

- [Build from source](#build-from-source)
- [Development](#development)
- [Where the commands come from](#where-the-commands-come-from)
- [Architecture](#architecture)
- [Compatibility](#compatibility)

## Build from source

Needs [Rust via rustup](https://rustup.rs). Nothing else — no second
repository, no token, no network beyond crates.io:

```sh
cargo build --release
./target/release/mapbox --help
```

## Development

```sh
cargo build
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

All four are a condition of a change landing, and CI runs the same ones.
`cargo fmt` and `cargo clippy` before the push rather than after it: the
lints below are denied in `Cargo.toml`, so a violation is a compile error
and not a style note.

`cargo test` never sends a request Mapbox reads — it answers to fake tokens
and a fake server, so the suite runs offline and on a machine that has never
logged in.

Four rules the compiler holds rather than a reviewer, declared in
`Cargo.toml` with the reasoning beside each: no `unsafe`, no `println!`
(stdout belongs to `output::emit`, the single place `--output` is honoured),
no `dbg!`, no `todo!`/`unimplemented!`.

## Where the commands come from

Commands are generated at build time from the OpenAPI specs in `openapi/`,
which are vendored: `src/spec.rs` names each one through `include_str!`, so
a clone compiles without fetching anything.

Those specs are derived rather than written here. They come from Mapbox's
own descriptions of its APIs through a maintainer-only regeneration step, so
an edit to `openapi/` is undone by the next sync — and what a Mapbox API
accepts is not something this repository decides in the first place. If a
command is missing a parameter the API supports, or describes one wrongly,
open an issue naming the operation and what it should be; the fix is made
where the specs are maintained, which is inside Mapbox.

`custom-openapi/` is the exception, and the one place a spec here is
editable: hand-authored specs for APIs Mapbox publishes no description for.
See its own README.

`build.rs` records which upstream commit the vendored specs came from, when
that information is available to it. It is best-effort and can never fail a
build — read its header before changing it.

## Architecture

See [docs/architecture.md](./docs/architecture.md) for the trust-boundary
and data-flow diagram (OAuth login, credential storage, API requests).

## Compatibility

While this is `0.x`, a breaking change bumps the minor version
(`0.1.5` → `0.2.0`); everything else bumps the patch. From 1.0 on, plain
[semver](https://semver.org/). Breaking: a command/flag removed, renamed,
or given a new default; an exit code or stdout/stderr change; a `--schema`
field removed or repurposed. Not breaking: new commands, new API response
fields, new `--schema` fields, and any wording.

Deprecated commands and endpoints print a warning before the request goes
out, and are marked `[deprecated]` in `--help` and `--schema`.

Official signed binaries are built and published by Mapbox from a separate,
private repository. Nothing about that pipeline is needed to build, test or
change the CLI, and nothing here depends on it.
