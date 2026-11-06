//! The diff plugins (design section 5.4; `versions::plugins`).
//!
//! The model:
//!
//! - Text: the changes, applied to the old lines, give the new lines. A `delete` names old line
//!   numbers, an `insert` and a `modify` name new line numbers; `old` and `new` hold the lines,
//!   joined with `\n`. The plugin promises this when the result is not `truncated` and no text
//!   was clipped (the texts here are short). Lines are compared after the plugin's
//!   normalisation: `\r\n` is `\n`, and a missing final newline is not a change.
//! - JSON: the changes, applied to the old value, give the new value. `modify` replaces the
//!   value at the path, `insert` adds a key or appends to an array, `delete` removes a key or the
//!   tail of an array. Key order and whitespace are not changes.
//! - Identical inputs give no changes (text, JSON, XML, binary), also after the normalisation
//!   (`\r\n` line ends in text; another layout of the same JSON value).

use proptest::prelude::*;
use ruum_core::versions::diff::{DiffChange, DiffChangeKind, DiffPlugin};
use ruum_core::versions::plugins::{BinaryPlugin, JsonPlugin, TextPlugin, XmlPlugin, text_diff};
use serde_json::{Map, Value};

// ------------------------------------------------------------------------------ text

fn line() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec!["a", "b", "c", "foo", "bar", " ", "  ", "é", "{", "}"]),
        0..6,
    )
    .prop_map(|w| w.concat())
}

/// Old lines, and new lines made from them by random edits (so the two share lines).
fn texts() -> impl Strategy<Value = (Vec<String>, Vec<String>)> {
    prop::collection::vec(line(), 0..14).prop_flat_map(|old| {
        let edits = prop::collection::vec((any::<prop::sample::Index>(), 0u8..3, line()), 0..8);
        (Just(old), edits).prop_map(|(old, edits)| {
            let mut new = old.clone();
            for (i, kind, l) in edits {
                match kind {
                    0 => new.insert(i.index(new.len() + 1), l),
                    1 if !new.is_empty() => {
                        new.remove(i.index(new.len()));
                    }
                    _ if !new.is_empty() => {
                        let i = i.index(new.len());
                        new[i] = l;
                    }
                    _ => new.push(l),
                }
            }
            (old, new)
        })
    })
}

/// The lines of a text as the plugin sees them.
fn keys(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split_inclusive('\n')
        .map(|l| l.trim_end_matches('\n').to_string())
        .collect()
}

/// `lines/3` or `lines/3-5` as (first index from 0, count).
fn line_range(path: &str) -> (usize, usize) {
    let r = path.strip_prefix("lines/").expect("a lines path");
    match r.split_once('-') {
        Some((a, b)) => {
            let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
            (a - 1, b - a + 1)
        }
        None => (r.parse::<usize>().unwrap() - 1, 1),
    }
}

fn split(s: &Option<String>) -> Vec<String> {
    s.as_deref()
        .expect("the change carries its lines")
        .split('\n')
        .map(str::to_string)
        .collect()
}

fn apply_text(old: &[String], changes: &[DiffChange]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut oi = 0;
    for c in changes {
        let (start, len) = line_range(c.path.as_deref().expect("a path"));
        match c.kind {
            DiffChangeKind::Delete => {
                out.extend_from_slice(&old[oi..start]);
                let gone = split(&c.old);
                assert_eq!(gone.len(), len, "{c:?}");
                assert_eq!(&old[start..start + len], &gone[..], "{c:?}");
                oi = start + len;
            }
            DiffChangeKind::Insert | DiffChangeKind::Modify => {
                while out.len() < start {
                    out.push(old[oi].clone());
                    oi += 1;
                }
                if c.kind == DiffChangeKind::Modify {
                    let gone = split(&c.old);
                    assert_eq!(&old[oi..oi + gone.len()], &gone[..], "{c:?}");
                    oi += gone.len();
                }
                let added = split(&c.new);
                assert_eq!(added.len(), len, "{c:?}");
                out.extend(added);
            }
            other => panic!("the text plugin made a {other:?} change"),
        }
    }
    out.extend_from_slice(&old[oi..]);
    out
}

fn join(lines: &[String], crlf: bool, final_newline: bool) -> String {
    let end = if crlf { "\r\n" } else { "\n" };
    let mut s = lines.join(end);
    if final_newline && !lines.is_empty() {
        s.push_str(end);
    }
    s
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn text_changes_applied_to_the_old_text_give_the_new(
        (old, new) in texts(),
        crlf in any::<(bool, bool)>(),
        nl in any::<(bool, bool)>(),
    ) {
        let (a, b) = (join(&old, crlf.0, nl.0), join(&new, crlf.1, nl.1));
        let r = TextPlugin.diff(a.as_bytes(), b.as_bytes()).unwrap();
        prop_assert!(!r.truncated);
        prop_assert_eq!(apply_text(&keys(&a), &r.changes), keys(&b), "{:?} -> {:?}: {:?}", a, b, r.changes);
        let s = r.summary;
        prop_assert_eq!((s.inserted + s.deleted + s.modified + s.moved) as usize, r.changes.len());
    }

    #[test]
    fn identical_texts_give_no_changes(lines in prop::collection::vec(line(), 0..14), nl in any::<bool>()) {
        let a = join(&lines, false, nl);
        prop_assert!(text_diff("text", &a, &a).changes.is_empty());
        // Line ends are normalised.
        let b = join(&lines, true, nl);
        prop_assert!(TextPlugin.diff(a.as_bytes(), b.as_bytes()).unwrap().changes.is_empty());
        prop_assert!(BinaryPlugin.diff(a.as_bytes(), a.as_bytes()).unwrap().changes.is_empty());
    }
}

// ------------------------------------------------------------------------------ JSON

fn key() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "a", "b", "c", "id", "0", "a b", "x.y", "", "é", "\"q\"", "[1]", "-_",
    ])
    .prop_map(str::to_string)
}

fn json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-3i64..4).prop_map(Value::from),
        prop::sample::select(vec!["", "x", "y", "a b"]).prop_map(Value::from),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map(key(), inner, 0..4)
                .prop_map(|m| Value::Object(m.into_iter().collect::<Map<_, _>>())),
        ]
    })
}

#[derive(Debug)]
enum Seg {
    Key(String),
    Idx(usize),
}

/// Parses `$.key[0]["odd key"]`.
fn json_path(path: &str) -> Vec<Seg> {
    let mut rest = path.strip_prefix('$').expect("a JSON path");
    let mut out = Vec::new();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let end = r
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .unwrap_or(r.len());
            out.push(Seg::Key(r[..end].to_string()));
            rest = &r[end..];
        } else if let Some(r) = rest.strip_prefix('[') {
            if r.starts_with('"') {
                let mut it = serde_json::Deserializer::from_str(r).into_iter::<String>();
                let k = it.next().expect("a key").expect("a JSON string");
                let used = it.byte_offset();
                out.push(Seg::Key(k));
                rest = r[used..].strip_prefix(']').expect("]");
            } else {
                let end = r.find(']').expect("]");
                out.push(Seg::Idx(r[..end].parse().expect("an index")));
                rest = &r[end + 1..];
            }
        } else {
            panic!("bad JSON path {path:?}");
        }
    }
    out
}

fn at<'v>(v: &'v mut Value, path: &[Seg]) -> &'v mut Value {
    path.iter().fold(v, |v, s| match s {
        Seg::Key(k) => v
            .get_mut(k.as_str())
            .unwrap_or_else(|| panic!("no key {k:?}")),
        Seg::Idx(i) => v.get_mut(*i).unwrap_or_else(|| panic!("no index {i}")),
    })
}

fn apply_json(old: &Value, changes: &[DiffChange]) -> Value {
    let mut v = old.clone();
    let parse = |s: &Option<String>| -> Value {
        serde_json::from_str(s.as_deref().expect("a value")).expect("the value is JSON")
    };
    // Deletes remove array tails, so they go last and from the end.
    let (deletes, others): (Vec<&DiffChange>, Vec<&DiffChange>) = changes
        .iter()
        .partition(|c| c.kind == DiffChangeKind::Delete);
    for c in others.into_iter().chain(deletes.into_iter().rev()) {
        let path = json_path(c.path.as_deref().expect("a path"));
        match c.kind {
            DiffChangeKind::Modify => {
                let target = at(&mut v, &path);
                assert_eq!(*target, parse(&c.old), "{c:?}");
                *target = parse(&c.new);
            }
            DiffChangeKind::Insert | DiffChangeKind::Delete => {
                let (last, up) = path.split_last().expect("not the root");
                let parent = at(&mut v, up);
                match (last, parent, c.kind) {
                    (Seg::Key(k), Value::Object(m), DiffChangeKind::Insert) => {
                        assert!(m.insert(k.clone(), parse(&c.new)).is_none(), "{c:?}");
                    }
                    (Seg::Key(k), Value::Object(m), _) => {
                        assert_eq!(m.remove(k), Some(parse(&c.old)), "{c:?}");
                    }
                    (Seg::Idx(i), Value::Array(a), DiffChangeKind::Insert) => {
                        assert_eq!(*i, a.len(), "{c:?}");
                        a.push(parse(&c.new));
                    }
                    (Seg::Idx(i), Value::Array(a), _) => {
                        assert_eq!(*i + 1, a.len(), "{c:?}");
                        assert_eq!(a.pop(), Some(parse(&c.old)), "{c:?}");
                    }
                    (s, p, _) => panic!("{c:?}: {s:?} in {p}"),
                }
            }
            other => panic!("the JSON plugin made a {other:?} change"),
        }
    }
    v
}

/// Another layout of the same value (indented, one key per line).
fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn json_changes_applied_to_the_old_value_give_the_new(old in json(), new in json()) {
        let (a, b) = (old.to_string(), new.to_string());
        let r = JsonPlugin.diff(a.as_bytes(), b.as_bytes()).unwrap();
        prop_assert!(!r.truncated);
        prop_assert_eq!(apply_json(&old, &r.changes), new);
    }

    #[test]
    fn identical_json_gives_no_changes(v in json()) {
        let compact = v.to_string();
        let r = JsonPlugin.diff(compact.as_bytes(), pretty(&v).as_bytes()).unwrap();
        prop_assert!(r.changes.is_empty(), "{:?}", r.changes);
        prop_assert_eq!(r.summary.inserted + r.summary.deleted + r.summary.modified, 0);
    }
}

// ------------------------------------------------------------------------------ XML

fn xml() -> impl Strategy<Value = String> {
    let name = prop::sample::select(vec!["a", "b", "item", "x-y"]);
    let text = prop::sample::select(vec!["", "t", "two words", " pad "]);
    let leaf = (name.clone(), text).prop_map(|(n, t)| format!("<{n}>{t}</{n}>"));
    leaf.prop_recursive(3, 16, 3, move |inner| {
        (
            name.clone(),
            prop::collection::btree_map(
                prop::sample::select(vec!["id", "k", "v"]),
                "[a-z]{0,3}",
                0..3,
            ),
            prop::collection::vec(inner, 0..3),
        )
            .prop_map(|(n, attrs, kids)| {
                let attrs: String = attrs.iter().map(|(k, v)| format!(" {k}=\"{v}\"")).collect();
                format!("<{n}{attrs}>{}</{n}>", kids.concat())
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn identical_xml_gives_no_changes(doc in xml()) {
        let r = XmlPlugin.diff(doc.as_bytes(), doc.as_bytes()).unwrap();
        prop_assert!(r.changes.is_empty(), "{:?}", r.changes);
    }
}
