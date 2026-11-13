//! A small reader for `app.yaml` (the YAML subset that a manifest needs), written by hand: the
//! workspace has no YAML library, and a new dependency needs the person (design 18.4).
//!
//! It reads block mappings and sequences, flow collections (`[a, b]`, `{k: v}`, also over several
//! lines, so a JSON manifest reads too), plain, single- and double-quoted scalars, block scalars
//! (`|`, `>`, with `-` or `+`), comments and one optional `---` at the start. Plain scalars
//! resolve as YAML 1.2 core: `null`/`~`/empty, `true`/`false`, integers and floats, else strings.
//!
//! The file comes from an archive, so it is untrusted: anchors, aliases, tags, directives,
//! complex keys and several documents are refused (no alias expansion, so no "billion laughs"),
//! as are duplicate keys, tabs in indentation, files over [`MAX_BYTES`], nesting deeper than
//! [`MAX_DEPTH`] and more than [`MAX_NODES`] values. An error names the line, never a value.

use serde_json::{Map, Number, Value};

/// The largest `app.yaml`.
pub const MAX_BYTES: usize = 256 * 1024;
/// The deepest nesting.
pub const MAX_DEPTH: usize = 32;
/// The most values.
pub const MAX_NODES: usize = 20_000;

/// Why `app.yaml` could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YamlError {
    /// The line (1-based), when known.
    pub line: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for YamlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "line {l}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

type R<T> = Result<T, YamlError>;

fn err<T>(line: Option<usize>, message: impl Into<String>) -> R<T> {
    Err(YamlError {
        line,
        message: message.into(),
    })
}

#[derive(Clone, Debug)]
struct Line<'a> {
    no: usize,
    indent: usize,
    /// The text after the indentation, without a comment and trailing spaces.
    text: String,
    /// The whole line as written (for block scalars).
    raw: &'a str,
}

/// Parses `app.yaml` into JSON.
pub fn parse(src: &str) -> R<Value> {
    if src.len() > MAX_BYTES {
        return err(None, format!("app.yaml is larger than {MAX_BYTES} bytes"));
    }
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut lines = Vec::new();
    let mut started = false;
    for (i, raw) in src.lines().enumerate() {
        let no = i + 1;
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        let rest = &raw[indent..];
        if rest.starts_with('\t') {
            return err(Some(no), "a tab in the indentation (use spaces)");
        }
        let text = strip_comment(rest).trim_end().to_string();
        if text.is_empty() {
            // Blank and comment lines matter only inside block scalars.
            lines.push(Line {
                no,
                indent: usize::MAX,
                text,
                raw,
            });
            continue;
        }
        if indent == 0 && text.starts_with('%') {
            return err(Some(no), "YAML directives are not read");
        }
        if indent == 0 && (text == "---" || text.starts_with("--- ")) {
            if started {
                return err(Some(no), "app.yaml holds one document only");
            }
            started = true;
            let after = text[3..].trim();
            if !after.is_empty() {
                return err(Some(no), "put the manifest on the lines after ---");
            }
            continue;
        }
        if indent == 0 && text == "..." {
            break;
        }
        started = true;
        lines.push(Line {
            no,
            indent,
            text,
            raw,
        });
    }
    let mut p = Parser {
        lines,
        pos: 0,
        nodes: 0,
    };
    p.skip_blank();
    if p.pos >= p.lines.len() {
        return Ok(Value::Null);
    }
    let first = p.lines[p.pos].indent;
    let v = p.node(first, 0)?;
    p.skip_blank();
    if let Some(l) = p.lines.get(p.pos) {
        return err(Some(l.no), "unexpected text (check the indentation)");
    }
    Ok(v)
}

/// The text before a comment: `#` at the start or after a space, outside quotes.
fn strip_comment(s: &str) -> &str {
    let b = s.as_bytes();
    let (mut single, mut double) = (false, false);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if double => i += 1,
            b'"' if !single => double = !double,
            b'\'' if !double => single = !single,
            b'#' if !single && !double && (i == 0 || b[i - 1] == b' ' || b[i - 1] == b'\t') => {
                return &s[..i];
            }
            _ => {}
        }
        i += 1;
    }
    s
}

struct Parser<'a> {
    lines: Vec<Line<'a>>,
    pos: usize,
    nodes: usize,
}

impl<'a> Parser<'a> {
    fn skip_blank(&mut self) {
        while self
            .lines
            .get(self.pos)
            .is_some_and(|l| l.indent == usize::MAX)
        {
            self.pos += 1;
        }
    }

    fn count(&mut self, line: usize) -> R<()> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return err(Some(line), format!("more than {MAX_NODES} values"));
        }
        Ok(())
    }

    /// The node that starts at the current line, whose indentation is `indent`.
    fn node(&mut self, indent: usize, depth: usize) -> R<Value> {
        self.skip_blank();
        let l = self.lines[self.pos].clone();
        if depth > MAX_DEPTH {
            return err(Some(l.no), format!("nesting deeper than {MAX_DEPTH}"));
        }
        if is_dash(&l.text) {
            self.seq(indent, depth)
        } else if map_key(&l.text, l.no)?.is_some() {
            self.map(indent, depth)
        } else {
            self.pos += 1;
            self.inline(&l, &l.text, depth)
        }
    }

    fn map(&mut self, indent: usize, depth: usize) -> R<Value> {
        let mut out = Map::new();
        loop {
            self.skip_blank();
            let Some(l) = self.lines.get(self.pos).cloned() else {
                break;
            };
            if l.indent < indent {
                break;
            }
            if l.indent > indent {
                return err(Some(l.no), "unexpected indentation");
            }
            let Some((key, rest)) = map_key(&l.text, l.no)? else {
                if is_dash(&l.text) {
                    return err(Some(l.no), "a list item where a key was expected");
                }
                return err(Some(l.no), "expected `key: value`");
            };
            self.count(l.no)?;
            if out.contains_key(&key) {
                return err(Some(l.no), "a key is given twice");
            }
            self.pos += 1;
            let value = self.value_after(&l, indent, &rest, depth)?;
            out.insert(key, value);
        }
        Ok(Value::Object(out))
    }

    /// The value of a mapping entry or list item whose text after the indicator is `rest`.
    fn value_after(&mut self, l: &Line<'a>, indent: usize, rest: &str, depth: usize) -> R<Value> {
        let rest = rest.trim();
        if rest.is_empty() {
            self.skip_blank();
            return match self.lines.get(self.pos) {
                Some(n) if n.indent > indent => {
                    let ni = n.indent;
                    self.node(ni, depth + 1)
                }
                // `key:` followed by `- item` at the same indentation.
                Some(n) if n.indent == indent && is_dash(&n.text) && !is_dash(&l.text) => {
                    self.seq(indent, depth + 1)
                }
                _ => Ok(Value::Null),
            };
        }
        if rest.starts_with('|') || rest.starts_with('>') {
            return self.block_scalar(l, indent, rest);
        }
        self.inline(l, rest, depth + 1)
    }

    fn seq(&mut self, indent: usize, depth: usize) -> R<Value> {
        let mut out = Vec::new();
        loop {
            self.skip_blank();
            let Some(l) = self.lines.get(self.pos).cloned() else {
                break;
            };
            if l.indent < indent || (l.indent == indent && !is_dash(&l.text)) {
                break;
            }
            if l.indent > indent {
                return err(Some(l.no), "unexpected indentation");
            }
            self.count(l.no)?;
            let item = l.text[1..].trim_start();
            if item.is_empty() {
                self.pos += 1;
                out.push(self.value_after(&l, indent, "", depth)?);
                continue;
            }
            // The item starts on the dash line: read it as if it began at its own column.
            let col = indent + (l.text.len() - item.len());
            self.lines[self.pos] = Line {
                no: l.no,
                indent: col,
                text: item.to_string(),
                raw: l.raw,
            };
            if depth + 1 > MAX_DEPTH {
                return err(Some(l.no), format!("nesting deeper than {MAX_DEPTH}"));
            }
            if item.starts_with('|') || item.starts_with('>') {
                self.pos += 1;
                out.push(self.block_scalar(&l, indent, item)?);
            } else {
                out.push(self.node(col, depth + 1)?);
            }
        }
        Ok(Value::Array(out))
    }

    /// A scalar or a flow collection that starts with `text` on line `l`; a flow collection may
    /// go on over the next lines.
    fn inline(&mut self, l: &Line<'a>, text: &str, depth: usize) -> R<Value> {
        let text = text.trim();
        if text.starts_with('[') || text.starts_with('{') {
            let mut whole = text.to_string();
            while !balanced(&whole) {
                self.skip_blank();
                let Some(n) = self.lines.get(self.pos) else {
                    return err(Some(l.no), "a [ or { is not closed");
                };
                whole.push(' ');
                whole.push_str(&n.text);
                self.pos += 1;
            }
            let mut f = Flow {
                s: whole.as_bytes(),
                i: 0,
                line: l.no,
                nodes: &mut self.nodes,
            };
            let v = f.value(depth)?;
            f.ws();
            if f.i != f.s.len() {
                return err(Some(l.no), "text after the end of a [ ] or { }");
            }
            return Ok(v);
        }
        check_plain_start(text, l.no)?;
        self.skip_blank();
        if let Some(n) = self.lines.get(self.pos)
            && n.indent > l.indent
            && n.indent != usize::MAX
        {
            return err(
                Some(n.no),
                "a value goes on over several lines; quote it or use | or >",
            );
        }
        scalar(text, l.no)
    }

    /// A block scalar (`|` keeps lines, `>` folds them) whose header is `head`, under a key or
    /// item at `indent`.
    fn block_scalar(&mut self, l: &Line<'a>, indent: usize, head: &str) -> R<Value> {
        let folded = head.starts_with('>');
        let mut chomp = ' ';
        for c in head[1..].chars() {
            match c {
                '-' | '+' => chomp = c,
                ' ' => {}
                _ => return err(Some(l.no), "only |, >, |-, |+, >- and >+ are read"),
            }
        }
        let mut body: Vec<&str> = Vec::new();
        let mut block_indent: Option<usize> = None;
        while let Some(n) = self.lines.get(self.pos) {
            let raw = n.raw;
            let ind = raw.len() - raw.trim_start_matches(' ').len();
            let blank = raw.trim().is_empty();
            if !blank {
                if ind <= indent {
                    break;
                }
                let bi = *block_indent.get_or_insert(ind);
                if ind < bi {
                    break;
                }
                body.push(&raw[bi..]);
            } else {
                body.push("");
            }
            self.pos += 1;
        }
        // Trailing blank lines belong to the chomping, not to the next key.
        let content_end = body
            .iter()
            .rposition(|s| !s.is_empty())
            .map_or(0, |i| i + 1);
        let trailing = body.len() - content_end;
        let lines = &body[..content_end];
        let mut text = if folded {
            let mut s = String::new();
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    s.push(if line.is_empty() || lines[i - 1].is_empty() {
                        '\n'
                    } else {
                        ' '
                    });
                }
                s.push_str(line);
            }
            s
        } else {
            lines.join("\n")
        };
        match chomp {
            '-' => {}
            '+' => {
                if !lines.is_empty() {
                    text.push('\n');
                }
                for _ in 0..trailing {
                    text.push('\n');
                }
            }
            _ => {
                if !lines.is_empty() {
                    text.push('\n');
                }
            }
        }
        Ok(Value::String(text))
    }
}

fn is_dash(t: &str) -> bool {
    t == "-" || t.starts_with("- ")
}

/// Brackets balance outside quotes.
fn balanced(s: &str) -> bool {
    let b = s.as_bytes();
    let (mut depth, mut single, mut double) = (0i64, false, false);
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if double => i += 1,
            b'"' if !single => double = !double,
            b'\'' if !double => single = !single,
            b'[' | b'{' if !single && !double => depth += 1,
            b']' | b'}' if !single && !double => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth <= 0 && !single && !double
}

/// The key and the rest of a block mapping line (`key: value` or `key:`), or `None` when the
/// line is not a mapping entry.
fn map_key(t: &str, line: usize) -> R<Option<(String, String)>> {
    if is_dash(t) || t.starts_with('[') || t.starts_with('{') {
        return Ok(None);
    }
    if t.starts_with('?') && (t.len() == 1 || t.as_bytes()[1] == b' ') {
        return err(Some(line), "complex keys (?) are not read");
    }
    if t.starts_with('"') || t.starts_with('\'') {
        let (key, used) = quoted(t, line)?;
        let rest = t[used..].trim_start();
        return match rest.strip_prefix(':') {
            Some(r) if r.is_empty() || r.starts_with(' ') => Ok(Some((key, r.to_string()))),
            _ => Ok(None),
        };
    }
    let b = t.as_bytes();
    for i in 0..b.len() {
        if b[i] == b':' && (i + 1 == b.len() || b[i + 1] == b' ') {
            let key = t[..i].trim_end();
            if key.is_empty() {
                return err(Some(line), "an empty key");
            }
            check_plain_start(key, line)?;
            return Ok(Some((key.to_string(), t[i + 1..].to_string())));
        }
    }
    Ok(None)
}

/// Refuses the YAML features that are not read, at the start of a plain scalar or key.
fn check_plain_start(t: &str, line: usize) -> R<()> {
    match t.as_bytes().first() {
        Some(b'&') => err(Some(line), "anchors (&) are not read"),
        Some(b'*') => err(Some(line), "aliases (*) are not read"),
        Some(b'!') => err(Some(line), "tags (!) are not read"),
        Some(b'@') | Some(b'`') => err(Some(line), "a plain value starts with @ or `; quote it"),
        Some(b'%') => err(Some(line), "a plain value starts with %; quote it"),
        _ => Ok(()),
    }
}

/// A quoted scalar at the start of `t`: its value and the bytes it used.
fn quoted(t: &str, line: usize) -> R<(String, usize)> {
    let b = t.as_bytes();
    let q = b[0];
    let mut out = String::new();
    let mut i = 1;
    if q == b'\'' {
        let start = i;
        let mut seg = start;
        while i < b.len() {
            if b[i] == b'\'' {
                if b.get(i + 1) == Some(&b'\'') {
                    out.push_str(&t[seg..=i]);
                    i += 2;
                    seg = i;
                    continue;
                }
                out.push_str(&t[seg..i]);
                return Ok((out, i + 1));
            }
            i += 1;
        }
        return err(Some(line), "a ' is not closed");
    }
    let mut seg = i;
    while i < b.len() {
        match b[i] {
            b'"' => {
                out.push_str(&t[seg..i]);
                return Ok((out, i + 1));
            }
            b'\\' => {
                out.push_str(&t[seg..i]);
                let Some(&c) = b.get(i + 1) else {
                    return err(Some(line), "a \\ at the end of a string");
                };
                i += 2;
                match c {
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'0' => out.push('\0'),
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b' ' => out.push(' '),
                    b'x' | b'u' | b'U' => {
                        let n = match c {
                            b'x' => 2,
                            b'u' => 4,
                            _ => 8,
                        };
                        let hex = t.get(i..i + n).ok_or_else(|| YamlError {
                            line: Some(line),
                            message: "a short \\x, \\u or \\U escape".into(),
                        })?;
                        let code = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
                        match code {
                            Some(ch) => out.push(ch),
                            None => return err(Some(line), "a bad \\x, \\u or \\U escape"),
                        }
                        i += n;
                    }
                    _ => return err(Some(line), "an unknown escape in a \"string\""),
                }
                seg = i;
            }
            _ => i += 1,
        }
    }
    err(Some(line), "a \" is not closed")
}

/// A block-context scalar (the whole `text`).
fn scalar(text: &str, line: usize) -> R<Value> {
    if text.starts_with('"') || text.starts_with('\'') {
        let (s, used) = quoted(text, line)?;
        if !text[used..].trim().is_empty() {
            return err(Some(line), "text after a quoted value");
        }
        return Ok(Value::String(s));
    }
    check_plain_start(text, line)?;
    if text.contains(": ") || text.ends_with(':') {
        return err(Some(line), "a `: ` inside a plain value; quote the value");
    }
    Ok(resolve(text))
}

/// The YAML 1.2 core type of a plain scalar.
fn resolve(t: &str) -> Value {
    match t {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = t.trim_start_matches('+').parse::<i64>() {
            return Value::Number(n.into());
        }
        if let Ok(n) = t.trim_start_matches('+').parse::<u64>() {
            return Value::Number(n.into());
        }
    }
    if let Some(h) = t.strip_prefix("0x")
        && let Ok(n) = i64::from_str_radix(h, 16)
    {
        return Value::Number(n.into());
    }
    if let Some(o) = t.strip_prefix("0o")
        && let Ok(n) = i64::from_str_radix(o, 8)
    {
        return Value::Number(n.into());
    }
    let float_like = digits.bytes().all(|b| {
        b.is_ascii_digit() || b == b'.' || b == b'e' || b == b'E' || b == b'-' || b == b'+'
    }) && digits.bytes().any(|b| b.is_ascii_digit())
        && digits.contains(['.', 'e', 'E']);
    if float_like
        && let Ok(f) = t.parse::<f64>()
        && let Some(n) = Number::from_f64(f)
    {
        return Value::Number(n);
    }
    Value::String(t.to_string())
}

/// The flow parser (`[ ... ]`, `{ ... }`).
struct Flow<'s, 'n> {
    s: &'s [u8],
    i: usize,
    line: usize,
    nodes: &'n mut usize,
}

impl Flow<'_, '_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && (self.s[self.i] == b' ' || self.s[self.i] == b'\t') {
            self.i += 1;
        }
    }

    fn count(&mut self) -> R<()> {
        *self.nodes += 1;
        if *self.nodes > MAX_NODES {
            return err(Some(self.line), format!("more than {MAX_NODES} values"));
        }
        Ok(())
    }

    fn value(&mut self, depth: usize) -> R<Value> {
        if depth > MAX_DEPTH {
            return err(Some(self.line), format!("nesting deeper than {MAX_DEPTH}"));
        }
        self.ws();
        self.count()?;
        match self.s.get(self.i) {
            Some(b'[') => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    self.ws();
                    if self.s.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(Value::Array(out));
                    }
                    out.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {}
                        _ => return err(Some(self.line), "expected , or ] in a list"),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut out = Map::new();
                loop {
                    self.ws();
                    if self.s.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Ok(Value::Object(out));
                    }
                    let key = match self.scalar(true)? {
                        Value::String(s) => s,
                        Value::Null => return err(Some(self.line), "an empty key"),
                        other => other.to_string(),
                    };
                    self.ws();
                    let value = if self.s.get(self.i) == Some(&b':') {
                        self.i += 1;
                        self.ws();
                        if matches!(self.s.get(self.i), Some(b',') | Some(b'}')) {
                            Value::Null
                        } else {
                            self.value(depth + 1)?
                        }
                    } else {
                        Value::Null
                    };
                    if out.insert(key, value).is_some() {
                        return err(Some(self.line), "a key is given twice");
                    }
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {}
                        _ => return err(Some(self.line), "expected , or } in a mapping"),
                    }
                }
            }
            _ => self.scalar(false),
        }
    }

    /// A scalar in a flow collection; `key` stops a plain scalar at `:` too.
    fn scalar(&mut self, key: bool) -> R<Value> {
        self.ws();
        let rest = std::str::from_utf8(&self.s[self.i..]).map_err(|_| YamlError {
            line: Some(self.line),
            message: "not UTF-8".into(),
        })?;
        if rest.starts_with('"') || rest.starts_with('\'') {
            let (s, used) = quoted(rest, self.line)?;
            self.i += used;
            return Ok(Value::String(s));
        }
        let b = rest.as_bytes();
        let mut end = 0;
        while end < b.len() {
            let c = b[end];
            if c == b',' || c == b']' || c == b'}' || c == b'[' || c == b'{' {
                break;
            }
            if c == b':'
                && (key || end + 1 == b.len() || matches!(b[end + 1], b' ' | b',' | b']' | b'}'))
            {
                break;
            }
            end += 1;
        }
        let text = rest[..end].trim();
        check_plain_start(text, self.line)?;
        self.i += end;
        Ok(resolve(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(s: &str) -> Value {
        parse(s).unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    #[test]
    fn manifests_of_the_cases_read() {
        assert_eq!(p("runtime: static\n"), json!({ "runtime": "static" }));
        assert_eq!(
            p("runtime: static\nenv:\n  APP_TOKEN: x\n"),
            json!({ "runtime": "static", "env": { "APP_TOKEN": "x" } })
        );
        assert_eq!(
            p(
                "runtime: static\nroles:\n  member:\n    description: Uses the plant list\n    \
               default: true\n  admin:\n    description: Gives app roles\n    grants_roles: true\n"
            ),
            json!({ "runtime": "static", "roles": {
                "member": { "description": "Uses the plant list", "default": true },
                "admin": { "description": "Gives app roles", "grants_roles": true },
            } })
        );
        assert_eq!(
            p("runtime: static\ncheck:\n  routes: [/missing.html]\n"),
            json!({ "runtime": "static", "check": { "routes": ["/missing.html"] } })
        );
        assert_eq!(
            p("runtime: static\nsecrets: [STRIPE_KEY]\n"),
            json!({ "runtime": "static", "secrets": ["STRIPE_KEY"] })
        );
        assert_eq!(
            p("runtime: static\nstatic:\n  root: '../../outside'\n"),
            json!({ "runtime": "static", "static": { "root": "../../outside" } })
        );
        assert_eq!(
            p("static:\n  root: 'dist\\..\\..\\outside'\n"),
            json!({ "static": { "root": "dist\\..\\..\\outside" } })
        );
        assert_eq!(
            p(
                "apis:\n  orders:\n    visibility: internal\n    protocol: http\n    spec: \
               '../orders.yaml'\n    scopes: [orders.read]\n"
            ),
            json!({ "apis": { "orders": { "visibility": "internal", "protocol": "http",
                "spec": "../orders.yaml", "scopes": ["orders.read"] } } })
        );
        assert_eq!(
            p("bindings:\n  db:\n    node: node:0190\n    role: edit\n"),
            json!({ "bindings": { "db": { "node": "node:0190", "role": "edit" } } })
        );
        assert_eq!(p("start: node x.js\n"), json!({ "start": "node x.js" }));
    }

    #[test]
    fn lists_scalars_and_blocks() {
        assert_eq!(
            p(
                "# a comment\n---\na:\n- 1\n- two\n-   k: v\n    j: [1, {x: y}]\nb: ~\nc: 1.5\nd: \"q\\n\" # c\n"
            ),
            json!({ "a": [1, "two", { "k": "v", "j": [1, { "x": "y" }] }], "b": null, "c": 1.5, "d": "q\n" })
        );
        assert_eq!(
            p("a:\n  - - 1\n    - 2\n  -\n    b: c\n"),
            json!({ "a": [[1, 2], { "b": "c" }] })
        );
        assert_eq!(
            p("a: |\n  one\n  two\n\nb: >-\n  x\n  y\n"),
            json!({ "a": "one\ntwo\n", "b": "x y" })
        );
        assert_eq!(
            p("a: 'it''s'\nurl: http://x/y#z\n"),
            json!({ "a": "it's", "url": "http://x/y#z" })
        );
        // JSON is YAML, also over several lines.
        assert_eq!(
            p("{\n  \"runtime\": \"static\",\n  \"env\": {\"A\": \"1\"}\n}\n"),
            json!({ "runtime": "static", "env": { "A": "1" } })
        );
        assert_eq!(p(""), Value::Null);
    }

    #[test]
    fn unsafe_and_unknown_forms_are_refused() {
        for bad in [
            "a: &x 1\nb: *x\n",
            "a: !!str 1\n",
            "%YAML 1.2\n---\na: 1\n",
            "a: 1\n---\nb: 2\n",
            "a: 1\na: 2\n",
            "a:\n\tb: 1\n",
            "? a\n: 1\n",
            "a: [1, 2\n",
            "a: x\n  y\n",
            "a: 1\n  b: 2\n",
            "- a\nb: 1\n",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(parse(&deep).is_err());
        let big = "a: b\n".repeat(MAX_BYTES / 4);
        assert!(parse(&big).is_err());
    }

    #[test]
    fn errors_do_not_quote_values() {
        let e = parse("token: \"sk_live_secret\\q\"\n").unwrap_err();
        assert!(!e.to_string().contains("sk_live_secret"), "{e}");
    }
}
