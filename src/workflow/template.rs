//! `${{ … }}` expressions: how one step's values reach the next.
//!
//! Deliberately a lookup language and nothing more — no operators, no
//! functions, no conditionals. An expression names a value: an input, or a
//! path into an earlier step's output. Anything that needs logic belongs in a
//! script step, which is a real language and is tested as one.
//!
//! A string that is exactly one expression takes the value's own JSON type,
//! so an object can be handed to `stdin` whole and a number stays a number.
//! An expression inside a longer string is interpolated as text.

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

const OPEN: &str = "${{";
const CLOSE: &str = "}}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Input(String),
    Step { id: String, path: Vec<Segment> },
}

/// What an expression can see while a workflow runs.
pub struct Context<'a> {
    pub inputs: &'a Map<String, Value>,
    /// The outputs of the steps that have finished, by id.
    pub steps: &'a BTreeMap<String, Value>,
}

/// Every reference in `value`, for checking before anything runs.
pub fn references(value: &Value) -> Result<Vec<Reference>, String> {
    let mut found = vec![];
    walk_strings(value, &mut |text| {
        for piece in pieces(text)? {
            if let Piece::Expression(reference) = piece {
                found.push(reference);
            }
        }
        Ok(())
    })?;
    Ok(found)
}

/// `value` with every expression replaced by what it names.
pub fn resolve(value: &Value, context: &Context) -> Result<Value> {
    match value {
        Value::String(text) => resolve_string(text, context),
        Value::Array(items) => items
            .iter()
            .map(|item| resolve(item, context))
            .collect::<Result<_>>()
            .map(Value::Array),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, item)| Ok((key.clone(), resolve(item, context)?)))
            .collect::<Result<_>>()
            .map(Value::Object),
        other => Ok(other.clone()),
    }
}

/// How a resolved value is spelled where only text fits: an argv entry, an
/// interpolated string.
pub fn as_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Bool(_) | Value::Number(_) => Some(value.to_string()),
        Value::Array(_) | Value::Object(_) => Some(value.to_string()),
    }
}

fn resolve_string(text: &str, context: &Context) -> Result<Value> {
    let pieces = pieces(text).map_err(|e| anyhow!(e))?;
    if let [Piece::Expression(reference)] = pieces.as_slice() {
        return lookup(reference, context);
    }
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Text(literal) => out.push_str(&literal),
            Piece::Expression(reference) => {
                let value = lookup(&reference, context)?;
                // Interpolating a null would write "null" or nothing into the
                // middle of a URL or a name, and neither is what was meant.
                let text = as_text(&value).ok_or_else(|| {
                    anyhow!(
                        "`{}` is empty, so it cannot be written into \"{text}\"",
                        display(&reference)
                    )
                })?;
                out.push_str(&text);
            }
        }
    }
    Ok(Value::String(out))
}

fn lookup(reference: &Reference, context: &Context) -> Result<Value> {
    match reference {
        Reference::Input(name) => Ok(context.inputs.get(name).cloned().unwrap_or(Value::Null)),
        Reference::Step { id, path } => {
            let mut current = context
                .steps
                .get(id)
                .ok_or_else(|| anyhow!("`{}`: step `{id}` has not run", display(reference)))?;
            for segment in path {
                let next = match segment {
                    Segment::Key(key) => current.get(key.as_str()),
                    Segment::Index(index) => current.get(*index),
                };
                current = next.ok_or_else(|| {
                    anyhow!(
                        "`{}`: step `{id}`'s output has nothing at that path",
                        display(reference)
                    )
                })?;
            }
            Ok(current.clone())
        }
    }
}

/// An expression as the author wrote it, for error messages.
pub fn display(reference: &Reference) -> String {
    match reference {
        Reference::Input(name) => format!("inputs.{name}"),
        Reference::Step { id, path } => {
            let mut out = format!("steps.{id}.output");
            for segment in path {
                match segment {
                    Segment::Key(key) => {
                        out.push('.');
                        out.push_str(key);
                    }
                    Segment::Index(index) => out.push_str(&format!("[{index}]")),
                }
            }
            out
        }
    }
}

fn walk_strings(
    value: &Value,
    visit: &mut dyn FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    match value {
        Value::String(text) => visit(text),
        Value::Array(items) => items.iter().try_for_each(|item| walk_strings(item, visit)),
        Value::Object(fields) => fields
            .values()
            .try_for_each(|item| walk_strings(item, visit)),
        _ => Ok(()),
    }
}

#[derive(Debug, PartialEq)]
enum Piece {
    Text(String),
    Expression(Reference),
}

fn pieces(text: &str) -> Result<Vec<Piece>, String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        if start > 0 {
            out.push(Piece::Text(rest[..start].to_string()));
        }
        let after = &rest[start + OPEN.len()..];
        let end = after
            .find(CLOSE)
            .ok_or_else(|| format!("unclosed `{OPEN}` in \"{text}\""))?;
        out.push(Piece::Expression(parse(after[..end].trim())?));
        rest = &after[end + CLOSE.len()..];
    }
    if !rest.is_empty() {
        out.push(Piece::Text(rest.to_string()));
    }
    Ok(out)
}

/// `inputs.<name>` or `steps.<id>.output` followed by `.key` and `[n]`.
fn parse(expression: &str) -> Result<Reference, String> {
    let invalid = || {
        format!(
            "`{expression}` is not an expression this understands. Use \
             `inputs.<name>` or `steps.<id>.output`, optionally followed by \
             `.key` or `[index]`"
        )
    };

    let mut segments = vec![];
    let mut chars = expression.chars().peekable();
    let mut first = true;
    while chars.peek().is_some() {
        if !first {
            match chars.next() {
                Some('.') => {}
                Some('[') => {
                    let digits: String = chars.by_ref().take_while(|c| *c != ']').collect();
                    let index = digits.parse::<usize>().map_err(|_| invalid())?;
                    segments.push(Segment::Index(index));
                    continue;
                }
                _ => return Err(invalid()),
            }
        }
        first = false;
        let mut key = String::new();
        while let Some(c) = chars.peek() {
            if c.is_ascii_alphanumeric() || *c == '_' || *c == '-' {
                key.push(*c);
                chars.next();
            } else {
                break;
            }
        }
        if key.is_empty() {
            return Err(invalid());
        }
        segments.push(Segment::Key(key));
    }

    let mut segments = segments.into_iter();
    match (segments.next(), segments.next(), segments.next()) {
        (Some(Segment::Key(root)), Some(Segment::Key(name)), None) if root == "inputs" => {
            Ok(Reference::Input(name))
        }
        (Some(Segment::Key(root)), Some(Segment::Key(id)), Some(Segment::Key(output)))
            if root == "steps" && output == "output" =>
        {
            Ok(Reference::Step {
                id,
                path: segments.collect(),
            })
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context_resolve(value: Value) -> Result<Value> {
        let inputs = json!({ "style_id": "abc", "zoom": 12, "empty": null });
        let steps = BTreeMap::from([(
            "source".to_string(),
            json!({ "owner": "alice", "layers": [{ "id": "water" }] }),
        )]);
        resolve(
            &value,
            &Context {
                inputs: inputs.as_object().unwrap(),
                steps: &steps,
            },
        )
    }

    #[test]
    fn a_whole_expression_keeps_its_type() {
        assert_eq!(
            context_resolve(json!("${{ inputs.zoom }}")).unwrap(),
            json!(12)
        );
        assert_eq!(
            context_resolve(json!("${{ steps.source.output.layers[0] }}")).unwrap(),
            json!({ "id": "water" })
        );
    }

    #[test]
    fn an_embedded_expression_is_interpolated_as_text() {
        assert_eq!(
            context_resolve(json!(
                "styles/${{ steps.source.output.owner }}/${{inputs.style_id}}"
            ))
            .unwrap(),
            json!("styles/alice/abc")
        );
    }

    #[test]
    fn objects_and_arrays_are_resolved_inside() {
        assert_eq!(
            context_resolve(json!({ "a": ["${{ inputs.style_id }}", 1] })).unwrap(),
            json!({ "a": ["abc", 1] })
        );
    }

    #[test]
    fn an_empty_value_cannot_be_interpolated() {
        assert!(context_resolve(json!("x-${{ inputs.empty }}")).is_err());
        assert_eq!(
            context_resolve(json!("${{ inputs.empty }}")).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn a_missing_path_names_the_expression() {
        let err = context_resolve(json!("${{ steps.source.output.nope }}")).unwrap_err();
        assert!(
            err.to_string().contains("steps.source.output.nope"),
            "{err}"
        );
    }

    #[test]
    fn references_are_found_for_checking() {
        assert_eq!(
            references(&json!({ "a": "${{ inputs.x }}", "b": ["${{ steps.s.output.k[2] }}"] }))
                .unwrap(),
            vec![
                Reference::Input("x".into()),
                Reference::Step {
                    id: "s".into(),
                    path: vec![Segment::Key("k".into()), Segment::Index(2)]
                }
            ]
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            "${{ inputs }}",
            "${{ inputs.a.b }}",
            "${{ steps.s }}",
            "${{ steps.s.result }}",
            "${{ env.HOME }}",
            "${{ inputs.a + 1 }}",
            "${{ steps.s.output[x] }}",
            "${{ inputs.a",
        ] {
            assert!(references(&json!(bad)).is_err(), "{bad} was accepted");
        }
    }
}
