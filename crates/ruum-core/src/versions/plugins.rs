//! The built-in diff plugins of step 1 (`docs/DESIGN.md`, section 5.4): text, JSON, XML, image
//! and the binary fallback. Each one is a small pipeline: normalise (line endings, sorted keys,
//! attribute order and whitespace), then compare in its own model, then report in the one change
//! model ([`DiffResult`]).
//!
//! The WebAssembly plugin runtime (wasmtime, for third-party plugins) comes later: the
//! [`DiffPlugin`] trait is the border it will implement.

use super::diff::{ContentInfo, DiffChange, DiffChangeKind, DiffPlugin, DiffResult, DiffSummary};
use crate::error::{Error, Result};
use serde_json::Value;
use similar::{Algorithm, DiffOp};
use std::collections::BTreeMap;
use std::time::Duration;

/// The most changes a plugin reports; after that the result is `truncated`.
pub const MAX_CHANGES: usize = 1000;
/// The longest `old` or `new` text of one change, in characters.
pub const MAX_TEXT: usize = 2000;
/// How long the line diff may search for the best answer.
const DIFF_DEADLINE: Duration = Duration::from_secs(2);

/// All built-in plugins.
pub fn builtin() -> Vec<Box<dyn DiffPlugin>> {
    vec![
        Box::new(JsonPlugin),
        Box::new(XmlPlugin),
        Box::new(ImagePlugin),
        Box::new(TextPlugin),
        Box::new(BinaryPlugin),
    ]
}

fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_TEXT {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(MAX_TEXT).collect();
        out.push('…');
        out
    }
}

fn change(kind: DiffChangeKind, path: impl Into<String>) -> DiffChange {
    DiffChange {
        kind,
        path: Some(path.into()),
        label: None,
        old: None,
        new: None,
        change: None,
        detail: Vec::new(),
    }
}

/// Builds a result from the changes: counts the summary and cuts at [`MAX_CHANGES`].
fn result(plugin: &str, mut changes: Vec<DiffChange>, mut truncated: bool) -> DiffResult {
    if changes.len() > MAX_CHANGES {
        changes.truncate(MAX_CHANGES);
        truncated = true;
    }
    let mut summary = DiffSummary::default();
    for c in &changes {
        match c.kind {
            DiffChangeKind::Insert => summary.inserted += 1,
            DiffChangeKind::Delete => summary.deleted += 1,
            DiffChangeKind::Move => summary.moved += 1,
            DiffChangeKind::Modify | DiffChangeKind::Replace | DiffChangeKind::Format => {
                summary.modified += 1
            }
        }
    }
    DiffResult {
        plugin: plugin.to_string(),
        from: None,
        to: None,
        summary,
        changes,
        render: None,
        truncated,
    }
}

fn mime(info: &ContentInfo<'_>) -> String {
    info.content_type
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn extension(info: &ContentInfo<'_>) -> String {
    info.name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------- text

/// Lines and words. Line endings are normalised (`\r\n` is `\n`).
pub struct TextPlugin;

impl TextPlugin {
    pub const ID: &'static str = "text@1.0.0";
}

fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        // The head may end inside a character.
        Err(e) => e.error_len().is_none() && head.len() - e.valid_up_to() < 4,
    }
}

fn lines_path(start: usize, len: usize) -> String {
    if len <= 1 {
        format!("lines/{}", start + 1)
    } else {
        format!("lines/{}-{}", start + 1, start + len)
    }
}

/// Word changes inside one changed line block.
fn word_detail(old: &str, new: &str) -> Vec<DiffChange> {
    let diff = similar::TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .timeout(DIFF_DEADLINE)
        .diff_words(old, new);
    let mut out = Vec::new();
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        let o: String = o.filter_map(|i| diff.old_slice(i)).collect();
        let n: String = n.filter_map(|i| diff.new_slice(i)).collect();
        let kind = match tag {
            similar::DiffTag::Equal => continue,
            similar::DiffTag::Delete => DiffChangeKind::Delete,
            similar::DiffTag::Insert => DiffChangeKind::Insert,
            similar::DiffTag::Replace => DiffChangeKind::Replace,
        };
        out.push(DiffChange {
            kind,
            path: None,
            label: None,
            old: (!o.is_empty()).then(|| clip(&o)),
            new: (!n.is_empty()).then(|| clip(&n)),
            change: None,
            detail: Vec::new(),
        });
        if out.len() >= 50 {
            break;
        }
    }
    out
}

/// The line diff of two texts.
pub fn text_diff(plugin: &str, old: &str, new: &str) -> DiffResult {
    let old = old.replace("\r\n", "\n");
    let new = new.replace("\r\n", "\n");
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    // Compare without the line end, so a missing final newline is not a change of the line.
    let key = |l: &&str| l.trim_end_matches('\n').to_string();
    let ok: Vec<String> = old_lines.iter().map(key).collect();
    let nk: Vec<String> = new_lines.iter().map(key).collect();
    let deadline = std::time::Instant::now() + DIFF_DEADLINE;
    let ops = similar::capture_diff_slices_deadline(Algorithm::Myers, &ok, &nk, Some(deadline));
    let join = |v: &[String]| v.join("\n");
    let mut changes = Vec::new();
    for op in ops {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                let mut c = change(DiffChangeKind::Delete, lines_path(old_index, old_len));
                c.old = Some(clip(&join(&ok[old_index..old_index + old_len])));
                changes.push(c);
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                let mut c = change(DiffChangeKind::Insert, lines_path(new_index, new_len));
                c.new = Some(clip(&join(&nk[new_index..new_index + new_len])));
                changes.push(c);
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let o = join(&ok[old_index..old_index + old_len]);
                let n = join(&nk[new_index..new_index + new_len]);
                let mut c = change(DiffChangeKind::Modify, lines_path(new_index, new_len));
                c.detail = word_detail(&o, &n);
                c.old = Some(clip(&o));
                c.new = Some(clip(&n));
                changes.push(c);
            }
        }
    }
    result(plugin, changes, false)
}

impl DiffPlugin for TextPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn matches(&self, info: &ContentInfo<'_>) -> Option<u32> {
        if crate::blobs::is_text(info.content_type) {
            Some(10)
        } else if looks_like_text(info.head) {
            Some(5)
        } else {
            None
        }
    }

    fn diff(&self, old: &[u8], new: &[u8]) -> Result<DiffResult> {
        let o = std::str::from_utf8(old).map_err(|_| Error::Internal("not UTF-8".into()))?;
        let n = std::str::from_utf8(new).map_err(|_| Error::Internal("not UTF-8".into()))?;
        Ok(text_diff(Self::ID, o, n))
    }
}

// ---------------------------------------------------------------------------- JSON

/// JSON values: keys are compared without regard to order, arrays by position. Paths are
/// `$.key[0].other`.
pub struct JsonPlugin;

impl JsonPlugin {
    pub const ID: &'static str = "json@1.0.0";
}

fn json_key(path: &str, key: &str) -> String {
    let simple = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if simple {
        format!("{path}.{key}")
    } else {
        format!("{path}[{}]", Value::String(key.to_string()))
    }
}

fn json_text(v: &Value) -> String {
    clip(&serde_json::to_string(v).unwrap_or_default())
}

fn json_walk(path: &str, old: &Value, new: &Value, out: &mut Vec<DiffChange>) {
    if out.len() > MAX_CHANGES {
        return;
    }
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeMap<&String, ()> = a.keys().chain(b.keys()).map(|k| (k, ())).collect();
            for k in keys.keys() {
                let p = json_key(path, k);
                match (a.get(*k), b.get(*k)) {
                    (Some(x), Some(y)) => json_walk(&p, x, y, out),
                    (Some(x), None) => {
                        let mut c = change(DiffChangeKind::Delete, p);
                        c.old = Some(json_text(x));
                        out.push(c);
                    }
                    (None, Some(y)) => {
                        let mut c = change(DiffChangeKind::Insert, p);
                        c.new = Some(json_text(y));
                        out.push(c);
                    }
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for i in 0..a.len().max(b.len()) {
                let p = format!("{path}[{i}]");
                match (a.get(i), b.get(i)) {
                    (Some(x), Some(y)) => json_walk(&p, x, y, out),
                    (Some(x), None) => {
                        let mut c = change(DiffChangeKind::Delete, p);
                        c.old = Some(json_text(x));
                        out.push(c);
                    }
                    (None, Some(y)) => {
                        let mut c = change(DiffChangeKind::Insert, p);
                        c.new = Some(json_text(y));
                        out.push(c);
                    }
                    (None, None) => {}
                }
            }
        }
        (a, b) if a == b => {}
        (a, b) => {
            let mut c = change(DiffChangeKind::Modify, path);
            c.old = Some(json_text(a));
            c.new = Some(json_text(b));
            out.push(c);
        }
    }
}

impl DiffPlugin for JsonPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn matches(&self, info: &ContentInfo<'_>) -> Option<u32> {
        let m = mime(info);
        if m == "application/json" || m.ends_with("+json") || extension(info) == "json" {
            return Some(20);
        }
        let first = info.head.iter().find(|b| !b.is_ascii_whitespace());
        (matches!(first, Some(b'{') | Some(b'[')) && looks_like_text(info.head)).then_some(8)
    }

    fn diff(&self, old: &[u8], new: &[u8]) -> Result<DiffResult> {
        let parse = |b: &[u8]| -> Result<Value> {
            if b.iter().all(u8::is_ascii_whitespace) {
                return Ok(Value::Null);
            }
            serde_json::from_slice(b).map_err(|e| Error::Internal(format!("not JSON: {e}")))
        };
        let (a, b) = (parse(old)?, parse(new)?);
        let mut changes = Vec::new();
        json_walk("$", &a, &b, &mut changes);
        Ok(result(Self::ID, changes, false))
    }
}

// ---------------------------------------------------------------------------- XML

/// XML element trees. Normalised: attributes sorted, whitespace-only text dropped, text
/// trimmed, comments and processing instructions ignored. Children are matched by name and
/// position among the children of the same name. Paths are `/root/item[2]/@id`.
pub struct XmlPlugin;

impl XmlPlugin {
    pub const ID: &'static str = "xml@1.0.0";
}

#[derive(Debug, Default, PartialEq)]
struct Element {
    name: String,
    attrs: BTreeMap<String, String>,
    text: String,
    children: Vec<Element>,
}

fn parse_xml(bytes: &[u8]) -> Result<Element> {
    use quick_xml::events::Event;
    let bad = |e: &dyn std::fmt::Display| Error::Internal(format!("not XML: {e}"));
    let text = std::str::from_utf8(bytes).map_err(|e| bad(&e))?;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack: Vec<Element> = vec![Element::default()];
    let start = |e: &quick_xml::events::BytesStart<'_>| -> Result<Element> {
        let mut el = Element {
            name: e.name().as_ref().to_string(),
            ..Element::default()
        };
        for a in e.attributes() {
            let a = a.map_err(|e| bad(&e))?;
            let v = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| a.value.to_string());
            el.attrs.insert(a.key.as_ref().to_string(), v);
        }
        Ok(el)
    };
    loop {
        match reader.read_event().map_err(|e| bad(&e))? {
            Event::Start(e) => {
                let el = start(&e)?;
                stack.push(el);
            }
            Event::Empty(e) => {
                let el = start(&e)?;
                stack.last_mut().expect("stack").children.push(el);
            }
            Event::End(_) => {
                let el = stack.pop().expect("stack");
                if stack.is_empty() {
                    return Err(bad(&"unbalanced end tag"));
                }
                stack.last_mut().expect("stack").children.push(el);
            }
            Event::Text(t) => {
                let raw = t.as_ref().to_string();
                let s = quick_xml::escape::unescape(&raw)
                    .map(|s| s.into_owned())
                    .unwrap_or(raw);
                let s = s.trim();
                if !s.is_empty() {
                    let top = stack.last_mut().expect("stack");
                    if !top.text.is_empty() {
                        top.text.push(' ');
                    }
                    top.text.push_str(s);
                }
            }
            Event::CData(t) => {
                let s = t.as_ref().to_string();
                stack.last_mut().expect("stack").text.push_str(s.trim());
            }
            Event::GeneralRef(r) => {
                let name = r.as_ref().to_string();
                let s = quick_xml::escape::unescape(&format!("&{name};"))
                    .map(|s| s.into_owned())
                    .unwrap_or_default();
                stack.last_mut().expect("stack").text.push_str(&s);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err(bad(&"unclosed element"));
    }
    Ok(stack.pop().expect("stack"))
}

fn xml_summary(e: &Element) -> String {
    let mut s = format!("<{}", e.name);
    for (k, v) in &e.attrs {
        s.push_str(&format!(" {k}=\"{v}\""));
    }
    s.push('>');
    if !e.text.is_empty() {
        s.push_str(&e.text);
    }
    if !e.children.is_empty() {
        s.push_str(&format!("…({} children)", e.children.len()));
    }
    clip(&s)
}

/// The path step of each child: `name` or `name[n]` when the name repeats.
fn steps(children: &[Element]) -> Vec<(String, usize)> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    children
        .iter()
        .map(|c| {
            let n = seen.entry(c.name.as_str()).or_insert(0);
            *n += 1;
            (c.name.clone(), *n)
        })
        .collect()
}

fn xml_walk(path: &str, a: &Element, b: &Element, out: &mut Vec<DiffChange>) {
    if out.len() > MAX_CHANGES {
        return;
    }
    let keys: BTreeMap<&String, ()> = a
        .attrs
        .keys()
        .chain(b.attrs.keys())
        .map(|k| (k, ()))
        .collect();
    for k in keys.keys() {
        let p = format!("{path}/@{k}");
        match (a.attrs.get(*k), b.attrs.get(*k)) {
            (Some(x), Some(y)) if x != y => {
                let mut c = change(DiffChangeKind::Modify, p);
                c.old = Some(clip(x));
                c.new = Some(clip(y));
                out.push(c);
            }
            (Some(x), None) => {
                let mut c = change(DiffChangeKind::Delete, p);
                c.old = Some(clip(x));
                out.push(c);
            }
            (None, Some(y)) => {
                let mut c = change(DiffChangeKind::Insert, p);
                c.new = Some(clip(y));
                out.push(c);
            }
            _ => {}
        }
    }
    if a.text != b.text {
        let mut c = change(DiffChangeKind::Modify, format!("{path}/text()"));
        c.old = Some(clip(&a.text));
        c.new = Some(clip(&b.text));
        if !a.text.is_empty() && !b.text.is_empty() {
            c.detail = word_detail(&a.text, &b.text);
        }
        out.push(c);
    }
    let sa = steps(&a.children);
    let sb = steps(&b.children);
    let index_b: BTreeMap<&(String, usize), usize> =
        sb.iter().enumerate().map(|(i, s)| (s, i)).collect();
    let index_a: BTreeMap<&(String, usize), usize> =
        sa.iter().enumerate().map(|(i, s)| (s, i)).collect();
    let step_path = |(name, n): &(String, usize), count: usize| {
        if count > 1 || *n > 1 {
            format!("{path}/{name}[{n}]")
        } else {
            format!("{path}/{name}")
        }
    };
    let count = |s: &[(String, usize)], name: &str| s.iter().filter(|(n, _)| n == name).count();
    for (i, s) in sa.iter().enumerate() {
        let c = count(&sa, &s.0).max(count(&sb, &s.0));
        match index_b.get(s) {
            Some(j) => xml_walk(&step_path(s, c), &a.children[i], &b.children[*j], out),
            None => {
                let mut ch = change(DiffChangeKind::Delete, step_path(s, c));
                ch.old = Some(xml_summary(&a.children[i]));
                out.push(ch);
            }
        }
    }
    for (j, s) in sb.iter().enumerate() {
        if !index_a.contains_key(s) {
            let c = count(&sa, &s.0).max(count(&sb, &s.0));
            let mut ch = change(DiffChangeKind::Insert, step_path(s, c));
            ch.new = Some(xml_summary(&b.children[j]));
            out.push(ch);
        }
    }
}

impl DiffPlugin for XmlPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn matches(&self, info: &ContentInfo<'_>) -> Option<u32> {
        let m = mime(info);
        let ext = extension(info);
        if m == "application/xml"
            || m == "text/xml"
            || m.ends_with("+xml")
            || matches!(
                ext.as_str(),
                "xml" | "svg" | "xhtml" | "xsd" | "xsl" | "xslt" | "bpmn" | "xliff" | "plist"
            )
        {
            return Some(20);
        }
        let start = info
            .head
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .map(|i| &info.head[i..]);
        start
            .is_some_and(|s| s.starts_with(b"<?xml") && looks_like_text(info.head))
            .then_some(8)
    }

    fn diff(&self, old: &[u8], new: &[u8]) -> Result<DiffResult> {
        let a = parse_xml(old)?;
        let b = parse_xml(new)?;
        let mut changes = Vec::new();
        xml_walk("", &a, &b, &mut changes);
        Ok(result(Self::ID, changes, false))
    }
}

// ---------------------------------------------------------------------------- images

/// Pixels: the size, the format, and how many pixels differ, with the box around them.
pub struct ImagePlugin;

impl ImagePlugin {
    pub const ID: &'static str = "image@1.0.0";
}

/// The most pixels the image plugin compares (64 megapixels per image).
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

impl DiffPlugin for ImagePlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn matches(&self, info: &ContentInfo<'_>) -> Option<u32> {
        let m = mime(info);
        let by_type = matches!(
            m.as_str(),
            "image/png" | "image/jpeg" | "image/webp" | "image/gif"
        );
        let by_bytes = image::guess_format(info.head).is_ok_and(|f| {
            matches!(
                f,
                image::ImageFormat::Png
                    | image::ImageFormat::Jpeg
                    | image::ImageFormat::WebP
                    | image::ImageFormat::Gif
            )
        });
        (by_type || by_bytes).then_some(20)
    }

    fn diff(&self, old: &[u8], new: &[u8]) -> Result<DiffResult> {
        let bad = |e: image::ImageError| Error::Internal(format!("not an image: {e}"));
        let fmt_a = image::guess_format(old).map_err(bad)?;
        let fmt_b = image::guess_format(new).map_err(bad)?;
        let a = image::load_from_memory_with_format(old, fmt_a).map_err(bad)?;
        let b = image::load_from_memory_with_format(new, fmt_b).map_err(bad)?;
        let mut changes = Vec::new();
        if fmt_a != fmt_b {
            let mut c = change(DiffChangeKind::Format, "image/format");
            c.change = Some(format!("{fmt_a:?} → {fmt_b:?}").to_lowercase());
            changes.push(c);
        }
        let (wa, ha) = (a.width(), a.height());
        let (wb, hb) = (b.width(), b.height());
        if (wa, ha) != (wb, hb) {
            let mut c = change(DiffChangeKind::Modify, "image/size");
            c.old = Some(format!("{wa}x{ha}"));
            c.new = Some(format!("{wb}x{hb}"));
            changes.push(c);
            return Ok(result(Self::ID, changes, false));
        }
        if u64::from(wa) * u64::from(ha) > MAX_PIXELS {
            return Ok(result(Self::ID, changes, true));
        }
        let a = a.to_rgba8();
        let b = b.to_rgba8();
        let (mut n, mut x0, mut y0, mut x1, mut y1) = (0u64, u32::MAX, u32::MAX, 0u32, 0u32);
        for (x, y, p) in a.enumerate_pixels() {
            if b.get_pixel(x, y) != p {
                n += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        if n > 0 {
            let total = u64::from(wa) * u64::from(ha);
            let mut c = change(DiffChangeKind::Modify, "image/pixels");
            c.label = Some(format!(
                "{n} of {total} pixels differ ({:.2}%)",
                n as f64 * 100.0 / total as f64
            ));
            c.detail.push(DiffChange {
                kind: DiffChangeKind::Modify,
                path: Some("image/region".into()),
                label: Some("the box around the changed pixels: x,y,width,height".into()),
                old: None,
                new: Some(format!("{x0},{y0},{},{}", x1 - x0 + 1, y1 - y0 + 1)),
                change: None,
                detail: Vec::new(),
            });
            changes.push(c);
        }
        Ok(result(Self::ID, changes, false))
    }
}

// ---------------------------------------------------------------------------- binary

/// The fallback for every content: "the files differ: 2.1 MB → 2.3 MB".
pub struct BinaryPlugin;

impl BinaryPlugin {
    pub const ID: &'static str = "binary@1.0.0";
}

/// A size for people: `512 B`, `2.1 KB`, `2.3 MB`, `1.2 GB` (powers of 1000).
pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// The binary result from the sizes and whether the contents are equal, without the bytes.
pub fn binary_result(old_size: u64, new_size: u64, equal: bool) -> DiffResult {
    let mut changes = Vec::new();
    if !equal {
        let mut c = change(DiffChangeKind::Modify, "content");
        c.label = Some(format!(
            "the files differ: {} → {}",
            human_size(old_size),
            human_size(new_size)
        ));
        c.old = Some(old_size.to_string());
        c.new = Some(new_size.to_string());
        changes.push(c);
    }
    result(BinaryPlugin::ID, changes, false)
}

impl DiffPlugin for BinaryPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn matches(&self, _info: &ContentInfo<'_>) -> Option<u32> {
        Some(0)
    }

    fn diff(&self, old: &[u8], new: &[u8]) -> Result<DiffResult> {
        Ok(binary_result(
            old.len() as u64,
            new.len() as u64,
            old == new,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info<'a>(name: &'a str, ct: Option<&'a str>, head: &'a [u8]) -> ContentInfo<'a> {
        ContentInfo {
            name,
            content_type: ct,
            head,
        }
    }

    #[test]
    fn text_reports_line_changes_with_word_detail() {
        let r = TextPlugin
            .diff(
                b"Intro\nPayment within 30 days.\nEnd\n",
                b"Intro\nPayment within 14 days.\nEnd\nNew line\n",
            )
            .unwrap();
        assert_eq!(r.plugin, "text@1.0.0");
        assert_eq!(r.summary.modified, 1);
        assert_eq!(r.summary.inserted, 1);
        let m = &r.changes[0];
        assert_eq!(m.kind, DiffChangeKind::Modify);
        assert_eq!(m.path.as_deref(), Some("lines/2"));
        assert_eq!(m.old.as_deref(), Some("Payment within 30 days."));
        assert_eq!(m.detail[0].kind, DiffChangeKind::Replace);
        assert_eq!(m.detail[0].old.as_deref(), Some("30"));
        assert_eq!(m.detail[0].new.as_deref(), Some("14"));
        assert_eq!(r.changes[1].path.as_deref(), Some("lines/4"));
    }

    #[test]
    fn text_ignores_line_endings() {
        let r = TextPlugin.diff(b"a\r\nb\r\n", b"a\nb\n").unwrap();
        assert!(r.changes.is_empty());
    }

    #[test]
    fn json_ignores_key_order_and_reports_paths() {
        let r = JsonPlugin
            .diff(
                br#"{"b": 1, "a": {"x": [1, 2]}, "gone": true}"#,
                br#"{"a": {"x": [1, 3, 4]}, "b": 1, "new key": null}"#,
            )
            .unwrap();
        let paths: Vec<_> = r
            .changes
            .iter()
            .map(|c| (c.kind, c.path.clone().unwrap()))
            .collect();
        assert_eq!(
            paths,
            vec![
                (DiffChangeKind::Modify, "$.a.x[1]".to_string()),
                (DiffChangeKind::Insert, "$.a.x[2]".to_string()),
                (DiffChangeKind::Delete, "$.gone".to_string()),
                (DiffChangeKind::Insert, "$[\"new key\"]".to_string()),
            ]
        );
        assert!(JsonPlugin.diff(b"{}", b"not json").is_err());
    }

    #[test]
    fn xml_ignores_attribute_order_and_whitespace() {
        let a = br#"<?xml version="1.0"?><root a="1" b="2"><item id="1">One</item><item id="2">Two</item></root>"#;
        let b = br#"<root b="2" a="1">
            <item id="1">One</item>
            <item id="2">Zwei</item>
            <extra/>
        </root>"#;
        let r = XmlPlugin.diff(a, b).unwrap();
        let paths: Vec<_> = r
            .changes
            .iter()
            .map(|c| (c.kind, c.path.clone().unwrap()))
            .collect();
        assert_eq!(
            paths,
            vec![
                (DiffChangeKind::Modify, "/root/item[2]/text()".to_string()),
                (DiffChangeKind::Insert, "/root/extra".to_string()),
            ]
        );
        assert!(XmlPlugin.diff(b"<a>", b"<a/>").is_err());
    }

    fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba(f(x, y)));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn image_counts_changed_pixels_and_sizes() {
        let a = png(10, 10, |_, _| [0, 0, 0, 255]);
        let b = png(10, 10, |x, y| {
            if (2..4).contains(&x) && y == 5 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 255]
            }
        });
        assert_eq!(ImagePlugin.matches(&info("a.png", None, &a)), Some(20));
        let r = ImagePlugin.diff(&a, &b).unwrap();
        assert_eq!(r.summary.modified, 1);
        assert_eq!(r.changes[0].detail[0].new.as_deref(), Some("2,5,2,1"));
        let c = png(5, 10, |_, _| [0, 0, 0, 255]);
        let r = ImagePlugin.diff(&a, &c).unwrap();
        assert_eq!(r.changes[0].path.as_deref(), Some("image/size"));
        assert_eq!(r.changes[0].new.as_deref(), Some("5x10"));
    }

    #[test]
    fn binary_is_the_fallback() {
        assert_eq!(BinaryPlugin.matches(&info("x", None, &[0, 1])), Some(0));
        let r = BinaryPlugin.diff(&[0; 2100], &[1; 2300]).unwrap();
        assert_eq!(
            r.changes[0].label.as_deref(),
            Some("the files differ: 2.1 KB → 2.3 KB")
        );
        assert!(
            BinaryPlugin
                .diff(b"same", b"same")
                .unwrap()
                .changes
                .is_empty()
        );
    }

    #[test]
    fn plugins_match_by_type_extension_and_content() {
        assert!(
            TextPlugin
                .matches(&info("a.bin", None, &[0, 1, 2]))
                .is_none()
        );
        assert_eq!(
            TextPlugin.matches(&info("notes", Some("text/plain"), b"hi")),
            Some(10)
        );
        assert_eq!(JsonPlugin.matches(&info("a.json", None, b"{}")), Some(20));
        assert_eq!(XmlPlugin.matches(&info("a.svg", None, b"<svg/>")), Some(20));
        assert_eq!(
            XmlPlugin.matches(&info("a", None, b"<?xml version='1.0'?><a/>")),
            Some(8)
        );
    }
}
