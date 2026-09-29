//! Renders a workflow's `description` for `workflow show`.
//!
//! A deliberately small subset of Markdown, the parts a description needs
//! and nothing a terminal cannot show: paragraphs separated by a blank line,
//! lines indented by two or more spaces as commands, `- ` items as a list,
//! and `code` spans. Anything else is a paragraph, so no description can
//! fail to render — it can only render plainly.
//!
//! Paragraphs and list items are reflowed to [`WIDTH`], so an author's own
//! line breaks inside a YAML `|` block do not leave ragged lines. Commands
//! are kept exactly as written.

use crate::output::style;

/// Columns a paragraph is reflowed to. There is no terminal-size lookup in
/// this crate, and 80 columns is what every terminal shows.
pub const WIDTH: usize = 80;

const COMMAND_INDENT: &str = "    ";

enum Block {
    Paragraph(String),
    Commands(Vec<String>),
    List(Vec<String>),
}

fn is_command(line: &str) -> bool {
    line.starts_with("  ")
}

fn list_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))
}

fn blocks(text: &str) -> Vec<Block> {
    let mut out: Vec<Block> = vec![];
    for chunk in text.split("\n\n") {
        // Only blocks from this chunk may be continued: a blank line always
        // ends a list or a run of commands.
        let first = out.len();
        let mut paragraph: Vec<&str> = vec![];
        for line in chunk.lines().filter(|line| !line.trim().is_empty()) {
            let open = out.len() > first && paragraph.is_empty();
            match out.last_mut() {
                // An indented line under a list item continues the item.
                Some(Block::List(items)) if open && is_command(line) => {
                    let last = items.last_mut().expect("a list has an item");
                    last.push(' ');
                    last.push_str(line.trim());
                    continue;
                }
                _ => {}
            }
            if let Some(item) = list_item(line) {
                flush(&mut paragraph, &mut out);
                let continues = out.len() > first;
                match out.last_mut() {
                    Some(Block::List(items)) if continues => items.push(item.trim().to_string()),
                    _ => out.push(Block::List(vec![item.trim().to_string()])),
                }
            } else if is_command(line) {
                flush(&mut paragraph, &mut out);
                let continues = out.len() > first;
                match out.last_mut() {
                    Some(Block::Commands(commands)) if continues => {
                        commands.push(line.trim().to_string())
                    }
                    _ => out.push(Block::Commands(vec![line.trim().to_string()])),
                }
            } else {
                paragraph.push(line.trim());
            }
        }
        flush(&mut paragraph, &mut out);
    }
    out
}

fn flush(paragraph: &mut Vec<&str>, out: &mut Vec<Block>) {
    if !paragraph.is_empty() {
        out.push(Block::Paragraph(paragraph.join(" ")));
        paragraph.clear();
    }
}

/// Words laid into lines of at most `width` columns. A word longer than a
/// line gets a line to itself rather than being broken.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![];
    let mut line = String::new();
    for word in text.split_whitespace() {
        let fits = line.is_empty() || line.chars().count() + 1 + word.chars().count() <= width;
        if !fits {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Paints the `code` spans in one line, backticks included. `inside`
/// carries an open span over to the next line, since wrapping may split one.
fn paint_spans(line: &str, inside: &mut bool, color: bool) -> String {
    if !color {
        return line.to_string();
    }
    let mut out = String::new();
    if *inside {
        out.push_str(style::ACCENT);
    }
    for c in line.chars() {
        if c == '`' && !*inside {
            out.push_str(style::ACCENT);
            out.push(c);
        } else if c == '`' {
            out.push(c);
            out.push_str(style::RESET);
        } else {
            out.push(c);
        }
        if c == '`' {
            *inside = !*inside;
        }
    }
    if *inside {
        out.push_str(style::RESET);
    }
    out
}

fn reflowed(text: &str, first: &str, rest: &str, color: bool) -> Vec<String> {
    let width = WIDTH.saturating_sub(first.chars().count()).max(20);
    let mut inside = false;
    wrap(text, width)
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let lead = if index == 0 { first } else { rest };
            format!("{lead}{}", paint_spans(line, &mut inside, color))
        })
        .collect()
}

pub fn render(text: &str, color: bool) -> String {
    blocks(text)
        .into_iter()
        .map(|block| match block {
            Block::Paragraph(text) => reflowed(&text, "", "", color).join("\n"),
            Block::Commands(commands) => commands
                .iter()
                .map(|command| {
                    format!(
                        "{COMMAND_INDENT}{}",
                        style::paint(command, style::ACCENT, color)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Block::List(items) => items
                .iter()
                .flat_map(|item| reflowed(item, "  • ", "    ", color))
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTION: &str = "\
Reads a style with one credential profile and creates a copy of it with
another. Log in to each account first:

  mapbox auth login --profile source
  mapbox auth login --profile target

Not copied:
- tilesets and fonts that belong to the source account, which the target
  can read only if they are public
- the sprite
";

    #[test]
    fn paragraphs_reflow_commands_stay_and_lists_hang() {
        assert_eq!(
            render(DESCRIPTION, false),
            "\
Reads a style with one credential profile and creates a copy of it with another.
Log in to each account first:

    mapbox auth login --profile source
    mapbox auth login --profile target

Not copied:

  • tilesets and fonts that belong to the source account, which the target can
    read only if they are public
  • the sprite"
        );
    }

    #[test]
    fn a_command_right_under_a_paragraph_is_still_a_command() {
        assert_eq!(
            render("Log in first:\n  mapbox auth login\nThen run it.", false),
            "Log in first:\n\n    mapbox auth login\n\nThen run it."
        );
    }

    #[test]
    fn color_changes_nothing_but_the_escapes() {
        let colored = render("See `warnings` and `a b`.\n\n  mapbox x", true);
        assert!(colored.contains(style::ACCENT), "{colored:?}");
        assert_eq!(
            style::strip(&colored),
            render("See `warnings` and `a b`.\n\n  mapbox x", false)
        );
    }

    #[test]
    fn a_span_split_by_wrapping_stays_painted_on_both_lines() {
        let long = format!("{} `one two three`", "word ".repeat(15).trim_end());
        let colored = render(&long, true);
        let lines: Vec<&str> = colored.lines().collect();
        assert_eq!(lines.len(), 2, "{colored:?}");
        assert_eq!(style::strip(&colored), render(&long, false));
    }
}
