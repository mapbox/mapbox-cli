//! The `Link` response header (RFC 8288), which is how the Mapbox APIs say
//! a listing has another page.
//!
//! `styles list-styles`, `accounts list-tokens` and the other paginated
//! listings answer with one page and a header naming the next:
//!
//! ```text
//! Link: <https://api.mapbox.com/styles/v1/user?start=cjk2&limit=10>; rel="next"
//! ```
//!
//! Parsed rather than matched with a substring or a regex, because the two
//! things that would break a shortcut both occur in real headers: a URI may
//! contain a comma, which is also the delimiter between links, and `rel` may
//! carry a space-separated list of relation types rather than one word.
//! [`next_url`] is the only thing this module is for, so it does the least
//! parsing that gets that right.

/// The `rel="next"` target in a `Link` header, if it has one.
///
/// Returns a slice of the header, so the caller decides whether to own it.
/// `None` for an absent relation, a malformed header, or a link with no
/// `rel` — nothing here fails, because a header this CLI could not parse is
/// a reason to say nothing rather than a reason to stop.
pub fn next_url(header: &str) -> Option<&str> {
    links(header).find_map(|link| link.is_next().then_some(link.uri))
}

/// One `<uri>; param=value; …` entry.
struct Link<'a> {
    uri: &'a str,
    params: &'a str,
}

impl Link<'_> {
    /// Whether this link's `rel` names `next`.
    ///
    /// RFC 8288 §3.3: the value may be a space-separated list, and relation
    /// types are compared case-insensitively. `rel="prev next"` is therefore
    /// a next link, and `rel=NEXT` — legal unquoted, since `next` needs no
    /// quoting — is too.
    fn is_next(&self) -> bool {
        self.params.split(';').any(|param| {
            let Some((name, value)) = param.split_once('=') else {
                return false;
            };
            name.trim().eq_ignore_ascii_case("rel")
                && value
                    .trim()
                    .trim_matches('"')
                    .split_whitespace()
                    .any(|rel| rel.eq_ignore_ascii_case("next"))
        })
    }
}

/// Splits a `Link` header into its entries.
///
/// The delimiter is a comma, and a URI may contain one
/// (`?bbox=1,2,3,4` is ordinary in this API), so entries are found by their
/// angle brackets instead of by splitting the header: everything between
/// `<` and the next `>` is a URI, and everything from there to the following
/// `<` is that link's parameters.
fn links(header: &str) -> impl Iterator<Item = Link<'_>> {
    let mut rest = header;
    std::iter::from_fn(move || {
        let open = rest.find('<')?;
        let after_open = &rest[open + 1..];
        let close = after_open.find('>')?;
        let uri = &after_open[..close];

        let tail = &after_open[close + 1..];
        // Up to the next link, which is where this link's parameters end.
        // A comma inside the *parameters* would be inside a quoted string
        // (`title="a, b"`), and stopping at `<` rather than `,` means one
        // cannot end the entry early either.
        let (params, next) = match tail.find('<') {
            Some(at) => (&tail[..at], &tail[at..]),
            None => (tail, ""),
        };
        rest = next;
        // The comma that separated this link from the one after it is still
        // on the end, and it is the delimiter rather than part of the last
        // parameter — left on, `rel="next",` does not equal `next`.
        let params = params.trim().trim_end_matches(',');
        Some(Link { uri, params })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_next_link() {
        let header = r#"<https://api.mapbox.com/styles/v1/u?start=cjk2>; rel="next""#;
        assert_eq!(
            next_url(header),
            Some("https://api.mapbox.com/styles/v1/u?start=cjk2")
        );
    }

    #[test]
    fn picks_next_out_of_several_relations() {
        let header = concat!(
            r#"<https://api.mapbox.com/a?start=1>; rel="prev", "#,
            r#"<https://api.mapbox.com/a?start=3>; rel="next", "#,
            r#"<https://api.mapbox.com/a?start=9>; rel="last""#
        );
        assert_eq!(next_url(header), Some("https://api.mapbox.com/a?start=3"));
    }

    /// The reason this is a parser and not `header.split(',')`. A bounding
    /// box is four comma-separated numbers, and `geocoding` and `tilequery`
    /// both take one — splitting on commas would cut this URI into pieces
    /// and find no link at all.
    #[test]
    fn a_comma_inside_the_uri_does_not_end_the_link() {
        let header = r#"<https://api.mapbox.com/a?bbox=-1,2,-3,4&start=7>; rel="next""#;
        assert_eq!(
            next_url(header),
            Some("https://api.mapbox.com/a?bbox=-1,2,-3,4&start=7")
        );
    }

    /// RFC 8288 §3.3: a space-separated list of relation types, compared
    /// case-insensitively. Both halves of that matter here.
    #[test]
    fn a_relation_list_and_odd_casing_still_count() {
        for header in [
            r#"<https://api.mapbox.com/a>; rel="prev next""#,
            r#"<https://api.mapbox.com/a>; rel="NEXT""#,
            r#"<https://api.mapbox.com/a>; rel=next"#,
            r#"<https://api.mapbox.com/a>; REL="next""#,
        ] {
            assert_eq!(
                next_url(header),
                Some("https://api.mapbox.com/a"),
                "{header}"
            );
        }
    }

    #[test]
    fn other_parameters_alongside_rel_are_ignored() {
        let header = r#"<https://api.mapbox.com/a>; type="application/json"; rel="next""#;
        assert_eq!(next_url(header), Some("https://api.mapbox.com/a"));
    }

    /// The last page of a listing. The API still sends a `Link`, naming the
    /// pages behind rather than ahead — so "has a header" must not be read
    /// as "has more", which is the whole reason this returns an `Option`
    /// rather than a bool about the header's presence.
    #[test]
    fn no_next_relation_is_none() {
        let header = r#"<https://api.mapbox.com/a?start=1>; rel="prev""#;
        assert_eq!(next_url(header), None);
    }

    /// A relation that merely *contains* "next" is not `next`.
    #[test]
    fn a_longer_relation_name_is_not_next() {
        for header in [
            r#"<https://api.mapbox.com/a>; rel="nextpage""#,
            r#"<https://api.mapbox.com/a>; rel="mynext""#,
        ] {
            assert_eq!(next_url(header), None, "{header}");
        }
    }

    /// Nothing here panics or errors: a header this module cannot make sense
    /// of means the CLI says nothing about pagination, which is what it did
    /// before there was a parser at all.
    #[test]
    fn malformed_headers_are_none_rather_than_a_failure() {
        for header in [
            "",
            "not a link header",
            "<unclosed; rel=\"next\"",
            "rel=\"next\"",
            "<>",
            ";;;",
        ] {
            assert_eq!(next_url(header), None, "{header:?}");
        }
    }
}
