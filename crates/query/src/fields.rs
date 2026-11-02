//! Fields in shell-syntax text: which one is at a position, and putting one
//! into a filter while keeping the rest of the text as typed.

use bson::Bson;

use crate::format::{Style, format_value, write_key};
use crate::parser::{NodeKind, parse};

/// One step of a path into a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Index(usize),
}

/// The path to the innermost field or array item at byte `offset`, e.g.
/// `items.0.sku` as `[Key("items"), Index(0), Key("sku")]`. Empty when the
/// offset is not on a field. Cut or broken text gives what could be read.
pub fn path_at(text: &str, offset: usize) -> Vec<Segment> {
    let mut path = Vec::new();
    let parsed = parse(text);
    let Some(mut node) = parsed.root.as_ref() else {
        return path;
    };
    loop {
        match &node.kind {
            NodeKind::Object(members) => {
                let Some(member) = members.iter().find(|member| {
                    let end = member.value.span.end.max(member.key_span.end);
                    (member.key_span.start..=end).contains(&offset)
                }) else {
                    break;
                };
                path.push(Segment::Key(member.key.clone()));
                node = &member.value;
            }
            NodeKind::Array(items) => {
                let Some((index, item)) = items
                    .iter()
                    .enumerate()
                    .find(|(_, item)| (item.span.start..=item.span.end).contains(&offset))
                else {
                    break;
                };
                path.push(Segment::Index(index));
                node = item;
            }
            _ => break,
        }
    }
    path
}

/// A filter after [`set_field`]. `cursor` is the byte offset just after the
/// value it put in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterEdit {
    pub text: String,
    pub cursor: usize,
}

/// Puts `key: value` into the filter text. A top-level field with that key
/// gets the new value; otherwise the field goes after the last one, on its
/// own line when the fields are on separate lines. Broken text is changed as
/// far as its recovered parts allow. Text that isn't an object becomes
/// `{ key: value }`.
pub fn set_field(text: &str, key: &str, value: &Bson) -> FilterEdit {
    let value_text = format_value(value, Style::Compact);
    let mut member = String::new();
    write_key(&mut member, key);
    member.push_str(": ");
    member.push_str(&value_text);

    let parsed = parse(text);
    let Some((members, span)) = parsed.root.as_ref().and_then(|root| match &root.kind {
        NodeKind::Object(members) => Some((members, root.span.clone())),
        _ => None,
    }) else {
        return FilterEdit {
            cursor: 2 + member.len(),
            text: format!("{{ {member} }}"),
        };
    };

    if let Some(existing) = members.iter().rev().find(|m| m.key == key) {
        // `{ a: }` or `{ a }`: the value is missing, so write the colon too.
        let (range, insert) = if matches!(existing.value.kind, NodeKind::Missing) {
            let start = existing.key_span.end;
            (
                start..trimmed_end(text, start, existing.value.span.end),
                format!(": {value_text}"),
            )
        } else {
            (existing.value.span.clone(), value_text)
        };
        return replace(text, range, &insert, insert.len());
    }

    let Some(last) = members.last() else {
        // `{}`, `{  }` or an unclosed `{`.
        let open = span.start + 1;
        let closed = span.end > open && text[..span.end].ends_with('}');
        let close = if closed { span.end - 1 } else { span.end };
        let insert = if !text[open..close].trim().is_empty() {
            return replace(text, open..open, &format!(" {member}"), 1 + member.len());
        } else if closed {
            format!(" {member} ")
        } else {
            format!(" {member} }}")
        };
        return replace(text, open..close, &insert, 1 + member.len());
    };

    let end = if matches!(last.value.kind, NodeKind::Missing) {
        trimmed_end(text, last.key_span.end, last.value.span.end)
    } else {
        last.value.span.end
    };
    let rest = &text[end..];
    let separator = if text[span.start..last.key_span.start].contains('\n') {
        format!("\n{}", indent_at(text, last.key_span.start))
    } else {
        " ".to_owned()
    };
    if rest.trim_start().starts_with(',') {
        // Keep the trailing comma style: `{ a: 1, }`.
        let at = end + (rest.len() - rest.trim_start().len()) + 1;
        let insert = format!("{separator}{member},");
        replace(text, at..at, &insert, separator.len() + member.len())
    } else {
        let insert = format!(",{separator}{member}");
        replace(text, end..end, &insert, insert.len())
    }
}

/// `text` with `range` replaced by `insert`; the cursor goes `cursor` bytes
/// into the inserted text.
fn replace(text: &str, range: std::ops::Range<usize>, insert: &str, cursor: usize) -> FilterEdit {
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..range.start]);
    out.push_str(insert);
    out.push_str(&text[range.end..]);
    FilterEdit {
        text: out,
        cursor: range.start + cursor,
    }
}

/// `end`, moved back over whitespace but not before `start`.
fn trimmed_end(text: &str, start: usize, end: usize) -> usize {
    start + text[start..end].trim_end().len()
}

/// The whitespace that starts the line `offset` is on.
fn indent_at(text: &str, offset: usize) -> &str {
    let line_start = text[..offset].rfind('\n').map_or(0, |newline| newline + 1);
    let line = &text[line_start..offset];
    &line[..line.len() - line.trim_start().len()]
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    fn set(text: &str, key: &str, value: impl Into<Bson>) -> String {
        let edit = set_field(text, key, &value.into());
        let mut marked = edit.text.clone();
        marked.insert(edit.cursor, '|');
        marked
    }

    #[test]
    fn replaces_the_value_of_an_existing_top_level_field() {
        assert_eq!(
            set("{ status: 'open', age: { $gt: 3 } }", "status", "done"),
            "{ status: 'done'|, age: { $gt: 3 } }"
        );
        assert_eq!(
            set("{ 'a.b': 1 }", "a.b", doc! { "$ne": 2 }),
            "{ 'a.b': { $ne: 2 }| }"
        );
    }

    #[test]
    fn appends_a_new_field_after_the_last_one() {
        assert_eq!(
            set("{ age: { $gt: 3 } }", "status", "done"),
            "{ age: { $gt: 3 }, status: 'done'| }"
        );
        assert_eq!(set("{ a: 1, }", "b", 2), "{ a: 1, b: 2|, }");
        assert_eq!(
            set("{ a: { b: 1 } }", "a.b", 2),
            "{ a: { b: 1 }, 'a.b': 2| }"
        );
    }

    #[test]
    fn appends_on_its_own_line_in_multi_line_text() {
        assert_eq!(set("{\n  a: 1\n}", "b", 2), "{\n  a: 1,\n  b: 2|\n}");
        assert_eq!(
            set("{\n    a: 1,\n}", "b", 2),
            "{\n    a: 1,\n    b: 2|,\n}"
        );
    }

    #[test]
    fn fills_empty_objects_and_replaces_non_objects() {
        assert_eq!(set("{  }", "a", 1), "{ a: 1| }");
        assert_eq!(set("{}", "a", 1), "{ a: 1| }");
        assert_eq!(set("{", "a", 1), "{ a: 1| }");
        assert_eq!(set("", "a", 1), "{ a: 1| }");
        assert_eq!(set("  // note", "a", 1), "{ a: 1| }");
        assert_eq!(set("status", "a", 1), "{ a: 1| }");
        assert_eq!(set("[1]", "a", 1), "{ a: 1| }");
    }

    #[test]
    fn does_its_best_with_broken_text() {
        assert_eq!(set("{ status: }", "status", "done"), "{ status: 'done'| }");
        assert_eq!(set("{ status }", "status", "done"), "{ status: 'done'| }");
        assert_eq!(set("{ a: 1, b: }", "c", 2), "{ a: 1, b:, c: 2| }");
        assert_eq!(set("{ a: 1", "b", 2), "{ a: 1, b: 2|");
    }

    #[test]
    fn finds_the_path_at_an_offset() {
        let text = "{ a: 1, items: [ { sku: 'x' }, 7 ] }";
        let at = |needle: &str| path_at(text, text.find(needle).unwrap() + 1);
        let key = |k: &str| Segment::Key(k.to_owned());

        assert_eq!(at("a:"), [key("a")]);
        assert_eq!(at("1,"), [key("a")]);
        assert_eq!(at("sku"), [key("items"), Segment::Index(0), key("sku")]);
        assert_eq!(at("'x'"), [key("items"), Segment::Index(0), key("sku")]);
        assert_eq!(at("7"), [key("items"), Segment::Index(1)]);
        assert!(path_at(text, 0).is_empty());
    }
}
