//! The specs' Markdown descriptions, as `--help` text.
//!
//! The descriptions are written for docs.mapbox.com: headings, tables, curl
//! examples and `<Note>` blocks around the prose. Printed as they are, that
//! made `geocoder batch --help` 113 lines of markup, most of it about calling
//! the API by hand. [`intro`] keeps the opening prose, and [`render`] turns
//! its markup into plain text.
//!
//! Only spec text comes through here. Hand-written help is plain text with
//! its own line breaks, which Markdown would fold into one paragraph.
//! `--schema` keeps the source: an agent reads Markdown well and wants it
//! whole.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// The opening prose of `text`, as Markdown: paragraphs and lists up to the
/// first heading, code block, table, quote, rule or HTML block. What follows
/// one of those is reference material — option tables, worked examples,
/// billing notes — that the page linked from help covers in full.
pub fn intro(text: &str) -> &str {
    let mut depth = 0usize;
    let mut end = 0;
    for (event, range) in Parser::new_ext(text, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 && !matches!(tag, Tag::Paragraph | Tag::List(_)) {
                    break;
                }
                depth += 1;
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    end = range.end;
                }
            }
            Event::Html(_) | Event::Rule if depth == 0 => break,
            _ => {}
        }
    }
    text[..end].trim_end()
}

/// `text` with its Markdown rendered as plain text.
///
/// Plain rather than styled: clap's wrapping doubles every line break in a
/// paragraph that opens with a styled run right after a blank line (clap
/// 4.6), which is how `tilesets get-mvt`'s "**Style-optimized tiles**:"
/// paragraph came out. Markers are dropped, a link keeps its address in
/// parentheses since a terminal may not make it clickable, paragraphs are
/// separated by a blank line and list items start their own line.
pub fn render(text: &str) -> String {
    let mut out = String::new();
    let mut lists: Vec<Option<u64>> = vec![];
    let mut links: Vec<String> = vec![];
    let mut link_text_start = 0;

    for event in Parser::new_ext(text, Options::ENABLE_TABLES) {
        match event {
            Event::Start(Tag::Paragraph) | Event::Start(Tag::Heading { .. })
                if lists.is_empty() =>
            {
                separate(&mut out, "\n\n");
            }
            Event::End(TagEnd::Heading(_)) => out.push('\n'),
            Event::Start(Tag::List(first)) => {
                if lists.is_empty() {
                    separate(&mut out, "\n");
                }
                lists.push(first);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                separate(&mut out, "\n");
                out.push_str(&"  ".repeat(lists.len().saturating_sub(1)));
                match lists.last_mut() {
                    Some(Some(n)) => {
                        out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => out.push_str("- "),
                }
            }
            Event::Code(code) => out.push_str(&code),
            Event::Start(Tag::Link { dest_url, .. }) => {
                links.push(dest_url.to_string());
                link_text_start = out.len();
            }
            Event::End(TagEnd::Link) => {
                if let Some(url) = links.pop() {
                    if out[link_text_start..] != url {
                        out.push_str(&format!(" ({url})"));
                    }
                }
            }
            Event::Start(Tag::CodeBlock(_)) => separate(&mut out, "\n\n"),
            Event::Text(text) => out.push_str(&text),
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push('\n'),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Ends what came before with `gap` — unless nothing did, or it already ends
/// that way.
fn separate(out: &mut String, gap: &str) {
    let trimmed = out.trim_end_matches(' ').len();
    out.truncate(trimmed);
    if out.is_empty() || out.ends_with(gap) {
        return;
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out.push_str(gap);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> String {
        render(text)
    }

    #[test]
    fn the_intro_stops_at_the_first_reference_block() {
        let text = "Looks up a place.\n\n## Structured input\n\nMore.";
        assert_eq!(intro(text), "Looks up a place.");

        let text = "Up to 50 queries.\n\n<Note title=\"Billing\">\n  Each counts.\n</Note>\n";
        assert_eq!(intro(text), "Up to 50 queries.");

        let text = "Run it:\n\n```bash\ncurl …\n```\n";
        assert_eq!(intro(text), "Run it:");
    }

    /// A paragraph that ends in a colon keeps the list it introduces.
    #[test]
    fn the_intro_keeps_a_list() {
        let text = "Three kinds:\n\n  - **Public** (`pk`)\n  - **Secret** (`sk`)\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        assert_eq!(
            intro(text),
            "Three kinds:\n\n  - **Public** (`pk`)\n  - **Secret** (`sk`)"
        );
    }

    #[test]
    fn inline_markup_is_rendered_rather_than_printed() {
        assert_eq!(
            plain("The **Geocoding API** does _two_ things\nwith `q`."),
            "The Geocoding API does two things with q."
        );
        assert_eq!(
            plain("See [Slippy Map Tilenames](https://wiki.example/x)."),
            "See Slippy Map Tilenames (https://wiki.example/x)."
        );
        // The code spans a regex stripped into "and or x".
        assert_eq!(plain("`and` or `x`"), "and or x");
    }

    #[test]
    fn paragraphs_and_lists_keep_their_shape() {
        assert_eq!(
            plain("Two things:\n\n- Forward\n- Reverse\n\nAfter."),
            "Two things:\n- Forward\n- Reverse\n\nAfter."
        );
        assert_eq!(plain("Steps:\n\n1. One\n2. Two"), "Steps:\n1. One\n2. Two");
    }
}
