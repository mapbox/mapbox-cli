//! Holds CHANGELOG.md to the version in `Cargo.toml`.
//!
//! A release bumps the version and moves the `## Unreleased` entries under a
//! `## <version> - <date>` heading in the same commit. Nothing in the release
//! pipeline reads the changelog, so a tag cut from a commit that bumped the
//! version but skipped the heading would ship with every entry still under
//! Unreleased. Between releases the version names the last one, whose heading
//! is already there, so this only bites on the commit that bumps it.

/// The heading's date is checked for shape only. Whether it is the day the
/// release actually went out is not something a test can know.
fn is_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
}

#[test]
fn the_current_version_has_a_dated_changelog_heading() {
    let version = env!("CARGO_PKG_VERSION");
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/CHANGELOG.md");
    let changelog = std::fs::read_to_string(path).expect("read CHANGELOG.md");
    let prefix = format!("## {version} - ");
    let found = changelog
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .any(is_iso_date);
    assert!(
        found,
        "Cargo.toml says {version}, but CHANGELOG.md has no `## {version} - YYYY-MM-DD` \
         heading. A version bump moves the Unreleased entries under that heading in the \
         same commit."
    );
}

#[test]
fn is_iso_date_takes_only_that_shape() {
    assert!(is_iso_date("2026-09-22"));
    assert!(!is_iso_date("<date>"));
    assert!(!is_iso_date("2026-9-22"));
    assert!(!is_iso_date("2026-09-22 (yanked)"));
}
