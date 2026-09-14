//! Records which `openapi-specs` commit the vendored specs came from, and
//! nudges toward a refresh when that copy looks old.
//!
//! Both answers come from a `PINNED_SOURCE` file that ships with the specs —
//! **not** from asking git about a checkout that may not exist. That is the
//! whole point of vendoring: somebody who clones this crate and runs `cargo
//! build` has the specs in `openapi/`, and needs neither a second repository
//! nor access to one.
//!
//! It is a pin, not a byproduct: only a maintainer-only regeneration step
//! writes it, in the same run that derives `openapi/`, so the commit named
//! there is always the one `openapi/`'s content came from. The routine sync
//! updates the raw mirror and leaves both alone.
//!
//! The file lives *outside this crate*, one directory up, next to the
//! tooling that decides what `openapi/` becomes and only exists for a
//! maintainer checkout — a checkout of just this crate has no such
//! directory, and that is the ordinary case rather than an error: a
//! missing file leaves both variables unset, exactly like a malformed one.
//!
//! `MAPBOX_SPEC_COMMIT` reaches the crate through `option_env!`, and
//! `crate::generate_skills` prints it in the header of every generated skill
//! so a reader can tell which spec revision a description came from.
//! `MAPBOX_SPEC_AGE_DAYS` rides along with it for the contract tests, whose
//! failures are otherwise easy to read as "the page is wrong" when the real
//! answer is "these specs are three weeks old".
//!
//! Best-effort by design, and never a build failure. An explicit
//! `MAPBOX_SPEC_COMMIT` in the environment wins — a release pipeline knows
//! the revision it assembled — and a manifest that is missing or malformed
//! leaves both variables unset rather than refusing to compile a CLI that
//! works perfectly well without a seven-character string in a comment. The
//! skill's primary staleness signal is a content digest computed from the
//! embedded specs themselves; this is the convenience on top.
//!
//! The staleness warning is a separate concern from the maintainer-only
//! drift check: that one asks whether `openapi/` matches upstream, which
//! needs the upstream repository. This only reads a date out of a local file
//! and says when a sync is due.

use std::path::Path;

fn main() {
    let manifest = Path::new("../internal/openapi-command-config/PINNED_SOURCE");

    // The vendored specs and their provenance are ordinary build inputs now,
    // rather than a sibling checkout's `.git/HEAD`. The `..` reaches out of
    // the crate into a maintainer-only directory; a checkout that is only
    // this crate has no such directory, and every read below already treats
    // that as nothing to say rather than as a failure.
    //
    // Named as an input only when it is actually there. cargo reads a
    // `rerun-if-changed` path that does not exist as permanently dirty, so
    // declaring it unconditionally would rebuild the whole crate on every
    // `cargo build`, `test` and `clippy` — in exactly the checkout that has
    // no such file, which is every checkout of just this crate.
    if manifest.exists() {
        println!("cargo:rerun-if-changed={}", manifest.display());
    }
    println!("cargo:rerun-if-changed=openapi");
    println!("cargo:rerun-if-env-changed=MAPBOX_SPEC_COMMIT");

    // An explicit value wins: a release pipeline knows the revision it
    // assembled. Nothing past this point is needed then.
    if std::env::var_os("MAPBOX_SPEC_COMMIT").is_some() {
        return;
    }

    let Ok(text) = std::fs::read_to_string(manifest) else {
        return;
    };

    let mut commit = None;
    let mut committed_at = None;
    for line in text.lines() {
        let mut field = line.split_whitespace();
        match (field.next(), field.next()) {
            (Some("commit"), Some(value)) => commit = Some(value.to_string()),
            (Some("committed_at"), Some(value)) => committed_at = value.parse::<u64>().ok(),
            _ => {}
        }
    }

    if let Some(commit) = commit {
        // Short, because a person reads this in a generated header and the
        // full forty characters buy nothing there.
        let short: String = commit.chars().take(7).collect();
        println!("cargo:rustc-env=MAPBOX_SPEC_COMMIT={short}");
    }

    warn_if_stale(committed_at);
}

/// New API surface lands in `openapi-specs` at something closer to a monthly
/// rate, and the maintainer-only sync that regenerates `openapi/` from it
/// runs weekly, so two weeks without one is already long enough to be worth a
/// nudge rather than a surprise.
const STALE_AFTER_DAYS: u64 = 14;

fn warn_if_stale(committed_at: Option<u64>) {
    let Some(committed_at) = committed_at else {
        return;
    };
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    let age_days = now.as_secs().saturating_sub(committed_at) / 86_400;

    // Emitted whether or not it is stale, because the tests that read it need
    // to be able to say "and this is *not* why" as much as "this is probably
    // why" — see `spec_checkout_note` in tests/docs_contract.rs.
    println!("cargo:rustc-env=MAPBOX_SPEC_AGE_DAYS={age_days}");

    if age_days > STALE_AFTER_DAYS {
        println!(
            "cargo:warning=openapi/ was derived from an openapi-specs commit {age_days} days \
             old. Regenerating it is a maintainer-only step; open an issue if this \
             matters for your build."
        );
    }
}
