# Agent notes for mapbox-cli

A map, not a manual. Every module here opens with a `//!` block explaining
what it is for and what it refuses to do, and those are the authority — this
file exists so that you know which one to read, which invariants are held by
something other than a reviewer's memory, and which mistakes this repository
has already made once.

[CONTRIBUTING.md](CONTRIBUTING.md) is the human-facing version and covers the
same ground more briefly. [README.md](README.md) is the user-facing one.

## What is generated, and what only looks it

**The commands are generated at build time.** `src/spec.rs` names each
document in `openapi/` through `include_str!`, so the command tree is a
function of those specs: a spec change rebuilds the commands, and a spec
renamed upstream is a broken build rather than a silent gap. A clone compiles
with no network beyond crates.io and no second checkout.

**`openapi/` is derived. Do not edit it.** The documents come from Mapbox's
own API descriptions through a maintainer-only regeneration step, which also
writes the `PINNED_SOURCE` file `build.rs` reads. An edit here survives until
the next regeneration and no longer, so a fix to a description belongs
upstream, not in this directory. `custom-openapi/` is the same in spirit.

**`docs/commands.md` is written by hand, and that is the one that drifts.**
Nothing in this repository generates it. Its **Parameters** tables were
transcribed from the specs by a person and its **Outputs** blocks are real
captured responses, re-taken by hand. `tests/docs_contract.rs` holds it to the
surface the binary reports, which catches a command that vanished or was
renamed; it cannot catch a parameter description that quietly stopped being
true. Read that file's header before changing the page.

## The output contract

The one invariant to internalize before touching anything:

**stdout is the result. Everything else is stderr.** `output::emit` is the
single place a result is written and the single place `--output` is honored.
Progress, warnings, hints and errors go to stderr through `output::progress`
and friends, in both output modes, so that `mapbox … > file` produces a file
holding only the answer.

Three things enforce it, and they are there because a `println!` is such an
easy thing to add:

- `clippy::print_stdout` is denied in `Cargo.toml`, so the macros cannot come
  back.
- `only_output_completion_and_binary_responses_write_to_stdout` in
  `tests/source_guards.rs` catches the other way in — taking the handle
  directly.
- `tests/output_contract.rs` checks which stream each kind of output actually
  lands on, in a real child process.

Four modules may touch stdout, and the guard lists each with its reason:
`output.rs`, which is the machinery; `completion.rs`, because a shell script
wrapped in JSON is unsourceable; `executor.rs`, because a binary API response
wrapped in JSON is a corrupt PNG; and `telemetry.rs`, which does not write at
all and only reads `stdout().is_terminal()`. Adding a fifth means editing that
list, on purpose, in front of a reviewer.

## One HTTP client

`http::client_for` builds every request-sending client in the crate, because
that is where the token, the timeouts, the proxy handling and the `User-Agent`
are attached. A client built anywhere else is a request that arrives
anonymous, untimed, or without the caller's proxy — none of which fails
loudly. `no_module_builds_its_own_client` in `src/http.rs` holds it.

Timeouts are a budget per kind of payload rather than one number:
`Payload::Bounded` for something a command line can hold, `Payload::File` for
a transfer. `--timeout` and `MAPBOX_TIMEOUT` override both.

## Values that reach a URL

`executor.rs`'s `path_segment` percent-encodes `/`, `?`, `#` and `\` in path
parameters and refuses a value of `.` or `..`, including its `%2e` spellings.
This is a fix, not a precaution: values were substituted into a path template,
so one carrying URL syntax moved the request rather than naming a segment in
it — with the caller's token attached. See the 0.2.1 entry in
[CHANGELOG.md](CHANGELOG.md).

`update_check.rs` restricts the *shape* of a version string it reads from the
network or from its own on-disk cache rather than trying to sanitize the
content, for the same class of reason.

If you are adding something that puts a caller's value into a URL, a header or
a filename, assume this repository has been wrong about it before and look for
the existing helper.

## The guards

`tests/source_guards.rs` holds the invariants that no type and no lint can
express, by reading the source and failing on the pattern. They are blunt on
purpose: they do not prove a call is correct, they make it *conspicuous*, so
that adding one is a decision somebody made rather than a line nobody looked
at twice. Today they hold which modules may delete from the filesystem, which
may write to stdout, which must carry a request id into an error, that every
telemetry marker is disclosed in README.md, and that the prose is American
English.

Each keeps a list with a reason per entry, and each fails with a message
saying what to do. **If a guard fails, the fix is almost never to add
yourself to its list** — read the reason first. When it genuinely is, add the
entry *and* the sentence explaining it.

## What the tests can and cannot tell you

`cargo test` answers to fake tokens and a fake server. It runs offline, on a
machine that has never logged in, and it proves the CLI builds the request it
meant to and renders the answer it was handed. **It cannot prove Mapbox
accepts that request**, because nothing in it has ever sent one. Keep that
boundary in mind when a change is about what the API does rather than about
what this code does — the honest move is to run the command against the real
API by hand and say so, not to add a test that agrees with your assumption.

The pattern in this repository is unit tests for pure functions plus an
end-to-end file for anything whose promise involves a real process: which
stream, which exit code, what is on disk afterwards. `tests/output_contract.rs`,
`dry_run.rs`, `non_interactive.rs` and `schema_contract.rs` all open by saying
what their unit-test counterparts cannot reach. Follow that.

`scripts/test-install.sh` and `test-install.ps1` exercise the installers end
to end without touching the network. `scripts/test-completion.sh` does the
same for the completion scripts.

## Conventions

- **American English**, in comments and documentation as well as in anything
  the CLI prints. `prose_is_american_english` checks it. Two deliberate
  exemptions: fenced code blocks, because sample output and captured API
  responses are quoted rather than written, and the `cancelled` error code,
  which is a compatibility promise rather than a spelling.
- **Four rules the compiler holds rather than a reviewer**, declared in
  `Cargo.toml` with the reasoning beside each: no `unsafe`, no `println!`, no
  `dbg!`, no `todo!`/`unimplemented!`. `print_stderr` is deliberately *not*
  denied — progress belongs there.
- **The toolchain is pinned, and the floor is a different number.**
  `rust-toolchain.toml` names the exact Rust every clone and every workflow
  builds with; `rust-version` in `Cargo.toml` names the oldest Rust the crate
  still compiles on. They answer different questions and are meant to be far
  apart. rustup applies the pin on its own, so do not add a `rustup override`:
  it outranks the file and is invisible to everyone else. Bumping either is
  its own PR, carrying the evidence.
- **CI builds `--locked`.** What CI tests is the graph in `Cargo.lock`. Run
  without it locally, since a legitimate dependency change has to be able to
  write the lockfile, then commit the result in the same PR.
- **Comments explain why, not what.** The density here is high and
  deliberate: a comment that records the failure a line prevents is what stops
  the next person removing it. Match the surrounding style rather than the
  minimum.

## Compatibility and the changelog

Command names, flags, the two output modes and the exit codes are promises;
the Mapbox APIs' own response bodies are not. The rules are in
[CONTRIBUTING.md](CONTRIBUTING.md#compatibility).

[CHANGELOG.md](CHANGELOG.md) is written by hand, newest first, because the
commit subject rarely explains why a change matters. A user-visible change
needs an entry under `## Unreleased` saying what it means for a script that
already works. Error *message* text is not one of the promises, so a change to
message prose does not need one.

Machine-readable values are contracts even when they look like prose. The
`cancelled` error code is the standing example: it is documented in
`docs/commands.md` and asserted in `tests/non_interactive.rs`, so renaming it
is a breaking change rather than a tidy-up.

## Before you open a PR

```sh
cargo build
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

All four are a condition of landing, and CI runs the same ones.

Two habits this repository has learned the hard way, both worth more than the
four commands above:

**Verify the claim, not the diff.** A green suite proves the tests agree with
the code, which is a different statement from the change being right. If a PR
says an API returns a header, read the header off a real response. If it says
a fix prevents something, revert the fix and watch the test fail — twice in
this repository's short history a test passed for a reason unrelated to what
it claimed to check.

**Say what you did not check.** An honest gap named in the PR body is worth
more than a confident summary, and it is the thing a reviewer cannot recover
on their own.
