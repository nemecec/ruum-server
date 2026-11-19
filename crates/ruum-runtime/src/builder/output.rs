//! The build's output out of the sandbox (card R-2b, "Output out of the sandbox"; backlog S-42).
//!
//! The job writes one tar of its output folder (`tar -c -f - -C /work/app .`: a fixed argument,
//! so no file name is ever a word of a command line or a tar option) to its standard output.
//! The builder reads it here with its own reader: never a library that unpacks to disk, never a
//! link followed, and the whole stream under a byte cap (the job is stopped when it writes
//! more). File names are bytes: spaces, newlines, a leading `-`, `\`, `$(...)` and other
//! bytes stay data.
//!
//! The rule for each entry (README, "Builder"):
//!
//! - kept: regular files (mode reduced to `0o755` bits at most: no setuid, setgid, sticky or
//!   group and world write), folders, and symbolic links whose target is relative and stays
//!   inside the output tree (`node_modules/.bin/*`);
//! - **dropped** (and named in the build log): a name with a `..` segment, an absolute name, a
//!   name with an empty segment or a NUL byte, a symbolic link whose target is absolute or
//!   leaves the output tree, hard links, devices, FIFOs and every other entry type;
//! - **dropped** too: a name or a link target over [`MAX_NAME_BYTES`] (the kernel's `PATH_MAX`);
//! - **refused** (the build fails): a broken tar (bad checksum, an entry past the end), a tar
//!   without its end-of-archive marker (a cut stream must never become an image with files
//!   missing), a long-name or PAX record that no entry follows before that marker, a malformed
//!   PAX record, bytes other than zeros after that marker, more than [`MAX_ENTRIES`] entries
//!   (with the folders the reader adds above them), or names (with those folders) of more than
//!   [`MAX_NAME_TOTAL_BYTES`] in all. These bounds keep the reading and the layer linear in the
//!   output, and both are checked as the names are read, before the memory grows: a deep or a
//!   wide path cannot make the builder copy each of its parents (reviews of R-2b, Kimi run 12).
//!   A long name or link target is kept only up to one byte past [`MAX_NAME_BYTES`] (enough to
//!   drop it), so one huge record is never copied whole.
//!
//! The link rule is **lexical**: a target is resolved segment by segment on the names, not on
//! a disk. A `..` after a segment that is itself a kept link can therefore end elsewhere when
//! the kernel resolves it in the app's container (`a/b/s -> ../..` and `a/b/t -> s/../..`). The
//! builder never resolves a link itself and drops every entry below a link, so such a link can
//! only point elsewhere inside the app's own image, never into the builder or another tenant.
//!
//! A later entry of the same name replaces an earlier one (tar semantics).

use bytes::Bytes;
use std::collections::{BTreeMap, HashMap};

/// The longest name or link target kept (Linux `PATH_MAX`).
pub const MAX_NAME_BYTES: usize = 4096;
/// The most entries an output may hold.
pub const MAX_ENTRIES: usize = 500_000;
/// The most bytes of all names of an output, with the folders the layer adds above them.
pub const MAX_NAME_TOTAL_BYTES: usize = 64 * 1024 * 1024;
/// The most dropped entries kept by name (the rest are only counted).
pub const MAX_DROPPED_SHOWN: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    File(Bytes),
    Dir,
    Symlink(Vec<u8>),
}

/// One kept entry: a relative path of segments separated by `/`, no `.` or `..`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutEntry {
    pub path: Vec<u8>,
    pub mode: u32,
    pub kind: Kind,
}

/// The kept entries (sorted by path; every folder above an entry is an entry too) and the
/// dropped ones with the reason (the first [`MAX_DROPPED_SHOWN`]; `dropped_total` counts all).
#[derive(Debug, Default)]
pub struct Output {
    pub entries: Vec<OutEntry>,
    pub dropped: Vec<(String, &'static str)>,
    pub dropped_total: usize,
}

/// The dropped entries: the first few by name, all counted.
#[derive(Default)]
struct Dropped {
    shown: Vec<(String, &'static str)>,
    total: usize,
}

impl Dropped {
    fn push(&mut self, name: &[u8], why: &'static str) {
        self.total += 1;
        if self.shown.len() < MAX_DROPPED_SHOWN {
            self.shown.push((shown(name), why));
        }
    }
}

/// Shows a name in a log line: control bytes and bad UTF-8 escaped, at most 200 characters.
pub fn shown(name: &[u8]) -> String {
    String::from_utf8_lossy(name)
        .escape_debug()
        .take(200)
        .collect()
}

fn octal(field: &[u8]) -> Result<u64, String> {
    // GNU base-256 for large values.
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        let mut v: u64 = u64::from(field[0] & 0x7f);
        for b in &field[1..] {
            v = v
                .checked_mul(256)
                .and_then(|v| v.checked_add(u64::from(*b)))
                .ok_or("a size field overflows")?;
        }
        return Ok(v);
    }
    let s: Vec<u8> = field
        .iter()
        .copied()
        .take_while(|b| *b != 0)
        .filter(|b| *b != b' ')
        .collect();
    if s.is_empty() {
        return Ok(0);
    }
    let s = std::str::from_utf8(&s).map_err(|_| "a numeric field is not octal")?;
    u64::from_str_radix(s, 8).map_err(|_| "a numeric field is not octal".to_owned())
}

fn cstr(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    &field[..end]
}

/// A long name or link target as the reader keeps it: at most one byte past
/// [`MAX_NAME_BYTES`], so a longer one is still dropped as too long but never copied whole.
fn long_value(v: &[u8]) -> Vec<u8> {
    v[..v.len().min(MAX_NAME_BYTES + 1)].to_vec()
}

/// The `path` and `linkpath` of a PAX header, when it gives them.
type PaxNames = (Option<Vec<u8>>, Option<Vec<u8>>);

/// PAX records (`path` and `linkpath` only; values kept as [`long_value`]). A malformed record
/// refuses the output: a reader that skipped it would fall back to other metadata.
fn pax(data: &[u8]) -> Result<PaxNames, String> {
    let bad = || "the output has a malformed PAX record".to_owned();
    let (mut path, mut link) = (None, None);
    // Writers pad the record block with NUL bytes.
    let end = data.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
    let mut rest = &data[..end];
    while !rest.is_empty() {
        let sp = rest.iter().position(|b| *b == b' ').ok_or_else(bad)?;
        let len = std::str::from_utf8(&rest[..sp])
            .ok()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(bad)?;
        if len <= sp + 1 || len > rest.len() || rest[len - 1] != b'\n' {
            return Err(bad());
        }
        let rec = &rest[sp + 1..len - 1];
        let eq = rec.iter().position(|b| *b == b'=').ok_or_else(bad)?;
        match &rec[..eq] {
            b"path" => path = Some(long_value(&rec[eq + 1..])),
            b"linkpath" => link = Some(long_value(&rec[eq + 1..])),
            _ => {}
        }
        rest = &rest[len..];
    }
    Ok((path, link))
}

/// The checked relative form of a name, or why it is dropped. `Ok(None)`: the root itself.
fn clean(name: &[u8]) -> Result<Option<Vec<u8>>, &'static str> {
    if name.contains(&0) {
        return Err("a NUL byte in the name");
    }
    if name.starts_with(b"/") {
        return Err("an absolute name");
    }
    if name.len() > MAX_NAME_BYTES {
        return Err("a name over 4096 bytes");
    }
    let mut n = name;
    while let Some(r) = n.strip_prefix(b"./") {
        n = r;
    }
    let n = n.strip_suffix(b"/").unwrap_or(n);
    if n.is_empty() || n == b"." {
        return Ok(None);
    }
    for seg in n.split(|b| *b == b'/') {
        if seg.is_empty() || seg == b"." {
            return Err("an empty or '.' segment in the name");
        }
        if seg == b".." {
            return Err("a '..' segment in the name");
        }
    }
    Ok(Some(n.to_vec()))
}

/// Whether a symbolic link at `path` with `target` stays inside the tree (lexically; nothing
/// is resolved on a disk).
fn link_inside(path: &[u8], target: &[u8]) -> bool {
    if target.is_empty()
        || target.len() > MAX_NAME_BYTES
        || target.starts_with(b"/")
        || target.contains(&0)
    {
        return false;
    }
    let mut stack: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
    stack.pop(); // the link's own name
    for seg in target.split(|b| *b == b'/') {
        match seg {
            b"" | b"." => {}
            b".." => {
                if stack.pop().is_none() {
                    return false;
                }
            }
            s => stack.push(s),
        }
    }
    true
}

/// Reads the job's output tar (at most `cap` bytes were accepted before this call; `cap` here
/// bounds the sum of the file sizes again).
pub fn read_output(tar: Bytes, cap: u64) -> Result<Output, String> {
    let mut at = 0usize;
    let mut kept: BTreeMap<Vec<u8>, OutEntry> = BTreeMap::new();
    let mut dropped = Dropped::default();
    let mut long_name: Option<Vec<u8>> = None;
    let mut long_link: Option<Vec<u8>> = None;
    let mut total: u64 = 0;
    let mut headers: usize = 0;
    let mut kept_names: usize = 0;
    loop {
        if at + 512 > tar.len() {
            // No end-of-archive marker: the stream was cut (also at an entry boundary).
            return Err(if at == tar.len() {
                "the output ends without its end-of-archive marker".to_owned()
            } else {
                "the output ends in the middle of a header".to_owned()
            });
        }
        let h = &tar[at..at + 512];
        if h.iter().all(|b| *b == 0) {
            // The end-of-archive marker; tar pads its last record with zeros, nothing else may
            // follow (a second archive appended to the first is refused).
            if tar[at..].iter().any(|b| *b != 0) {
                return Err("the output has data after its end-of-archive marker".to_owned());
            }
            // A long name or link that no entry used: the entry it was for is missing.
            if long_name.is_some() || long_link.is_some() {
                return Err(
                    "the output ends after a long-name or PAX record without its entry".to_owned(),
                );
            }
            break;
        }
        headers += 1;
        if headers > 2 * MAX_ENTRIES {
            return Err(format!("the output has more than {MAX_ENTRIES} entries"));
        }
        let want = octal(&h[148..156])?;
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(*b)
                }
            })
            .sum();
        if want != sum {
            return Err("a header of the output has a bad checksum".to_owned());
        }
        let size = octal(&h[124..136])?;
        let typ = h[156];
        let data_start = at + 512;
        let data_end = data_start
            .checked_add(usize::try_from(size).map_err(|_| "an entry is too large")?)
            .ok_or("an entry is too large")?;
        if data_end > tar.len() {
            return Err("an entry of the output ends past the end".to_owned());
        }
        let data = tar.slice(data_start..data_end);
        at = data_end + ((512 - (size % 512) as usize) % 512);
        match typ {
            b'L' => {
                long_name = Some(long_value(cstr(&data)));
                continue;
            }
            b'K' => {
                long_link = Some(long_value(cstr(&data)));
                continue;
            }
            b'x' => {
                let (p, l) = pax(&data)?;
                if p.is_some() {
                    long_name = p;
                }
                if l.is_some() {
                    long_link = l;
                }
                continue;
            }
            b'g' => continue,
            _ => {}
        }
        let name = match long_name.take() {
            Some(n) => n,
            None => {
                let name = cstr(&h[0..100]);
                let prefix = if &h[257..263] == b"ustar\0" {
                    cstr(&h[345..500])
                } else {
                    &[]
                };
                if prefix.is_empty() {
                    name.to_vec()
                } else {
                    let mut p = prefix.to_vec();
                    p.push(b'/');
                    p.extend_from_slice(name);
                    p
                }
            }
        };
        let link = long_link
            .take()
            .unwrap_or_else(|| cstr(&h[157..257]).to_vec());
        let path = match clean(&name) {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(why) => {
                dropped.push(&name, why);
                continue;
            }
        };
        let mode = (octal(&h[100..108])? as u32) & 0o755;
        let kind = match typ {
            b'0' | 0 | b'7' => {
                total += size;
                if total > cap {
                    return Err(format!("the output is over {cap} bytes"));
                }
                Kind::File(data)
            }
            b'5' => Kind::Dir,
            b'2' => {
                if !link_inside(&path, &link) {
                    dropped.push(&path, "a link that leaves the output");
                    continue;
                }
                Kind::Symlink(link)
            }
            b'1' => {
                dropped.push(&path, "a hard link");
                continue;
            }
            b'3' | b'4' => {
                dropped.push(&path, "a device");
                continue;
            }
            b'6' => {
                dropped.push(&path, "a FIFO");
                continue;
            }
            _ => {
                dropped.push(&path, "an unknown entry type");
                continue;
            }
        };
        let mode = if matches!(kind, Kind::Dir) {
            0o755
        } else {
            mode | 0o400
        };
        let len = path.len();
        if kept
            .insert(path.clone(), OutEntry { path, mode, kind })
            .is_none()
        {
            // A new name: counted before the next one is read, so the names held here never
            // pass the bound (`complete` counts them again with the folders above them).
            kept_names += len;
            if kept_names > MAX_NAME_TOTAL_BYTES {
                return Err(format!(
                    "the output's names are over {MAX_NAME_TOTAL_BYTES} bytes in all as read"
                ));
            }
        }
        if kept.len() > MAX_ENTRIES {
            return Err(format!("the output has more than {MAX_ENTRIES} entries"));
        }
    }
    complete(kept, dropped)
}

/// What a name is in the finished tree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Node {
    /// A folder (kept or added above an entry).
    Dir,
    /// A file or a link: nothing may be below it.
    Leaf,
    /// Dropped (below a leaf): everything below it is dropped too.
    Gone,
}

/// Drops every entry below a file or a link (it would write through it at unpack time) and adds
/// each folder that is above an entry but not in the tar, so the layer needs no parent walk.
/// Linear: names are visited in sorted order (a parent sorts before what it holds), each name
/// looks up only its parent, and a missing parent is added once (memoized), under
/// [`MAX_NAME_TOTAL_BYTES`] and [`MAX_ENTRIES`]: every name of the finished tree (kept, added or
/// dropped below a leaf) counts, so a wide tree of short paths cannot add millions of folders
/// to the layer (Kimi, run 12).
fn complete(kept: BTreeMap<Vec<u8>, OutEntry>, mut dropped: Dropped) -> Result<Output, String> {
    let mut nodes: HashMap<Vec<u8>, Node> = HashMap::with_capacity(kept.len());
    let mut added: Vec<Vec<u8>> = Vec::new();
    let mut name_bytes: usize = 0;
    let over = || {
        format!(
            "the output's names are over {MAX_NAME_TOTAL_BYTES} bytes in all, with the folders above them"
        )
    };
    let too_many =
        || format!("the output has more than {MAX_ENTRIES} entries with the folders above them");
    let mut entries: Vec<OutEntry> = Vec::with_capacity(kept.len());
    for (path, e) in kept {
        // The node of the parent, adding the missing folders above it (top-down).
        let parent = match path.iter().rposition(|b| *b == b'/') {
            None => Node::Dir,
            Some(i) => {
                let mut missing: Vec<usize> = Vec::new();
                let mut cut = i;
                let mut found = Node::Dir;
                loop {
                    if let Some(n) = nodes.get(&path[..cut]) {
                        found = *n;
                        break;
                    }
                    missing.push(cut);
                    match path[..cut].iter().rposition(|b| *b == b'/') {
                        Some(j) => cut = j,
                        None => break,
                    }
                }
                for cut in missing.into_iter().rev() {
                    let node = if found == Node::Dir {
                        Node::Dir
                    } else {
                        Node::Gone
                    };
                    name_bytes += cut;
                    if name_bytes > MAX_NAME_TOTAL_BYTES {
                        return Err(over());
                    }
                    if nodes.len() >= MAX_ENTRIES {
                        return Err(too_many());
                    }
                    nodes.insert(path[..cut].to_vec(), node);
                    if node == Node::Dir {
                        added.push(path[..cut].to_vec());
                    }
                    found = node;
                }
                found
            }
        };
        name_bytes += path.len();
        if name_bytes > MAX_NAME_TOTAL_BYTES {
            return Err(over());
        }
        if nodes.len() >= MAX_ENTRIES {
            return Err(too_many());
        }
        if parent != Node::Dir {
            dropped.push(&path, "an entry below a file or a link");
            nodes.insert(path, Node::Gone);
            continue;
        }
        let node = if matches!(e.kind, Kind::Dir) {
            Node::Dir
        } else {
            Node::Leaf
        };
        nodes.insert(path, node);
        entries.push(e);
    }
    drop(nodes);
    entries.extend(added.into_iter().map(|path| OutEntry {
        path,
        mode: 0o755,
        kind: Kind::Dir,
    }));
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Output {
        entries,
        dropped: dropped.shown,
        dropped_total: dropped.total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GNU-style header (the format of busybox `tar -c`), with a long name record when needed.
    fn gnu(out: &mut Vec<u8>, name: &[u8], typ: u8, link: &[u8], data: &[u8]) {
        fn hdr(name: &[u8], typ: u8, link: &[u8], size: usize) -> [u8; 512] {
            let mut h = [0u8; 512];
            let n = name.len().min(100);
            h[..n].copy_from_slice(&name[..n]);
            h[100..107].copy_from_slice(b"0000644");
            h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
            h[156] = typ;
            let l = link.len().min(100);
            h[157..157 + l].copy_from_slice(&link[..l]);
            h[257..265].copy_from_slice(b"ustar  \0");
            h[148..156].copy_from_slice(b"        ");
            let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            h
        }
        let pad = |out: &mut Vec<u8>, n: usize| {
            out.extend(std::iter::repeat_n(0u8, (512 - n % 512) % 512))
        };
        if link.len() > 100 {
            let mut l = link.to_vec();
            l.push(0);
            out.extend_from_slice(&hdr(b"././@LongLink", b'K', b"", l.len()));
            out.extend_from_slice(&l);
            pad(out, l.len());
        }
        if name.len() > 100 {
            let mut n = name.to_vec();
            n.push(0);
            out.extend_from_slice(&hdr(b"././@LongLink", b'L', b"", n.len()));
            out.extend_from_slice(&n);
            pad(out, n.len());
        }
        out.extend_from_slice(&hdr(name, typ, link, data.len()));
        out.extend_from_slice(data);
        pad(out, data.len());
    }

    /// Name, type flag, link target, data.
    type Spec<'a> = (&'a [u8], u8, &'a [u8], &'a [u8]);

    fn read(entries: &[Spec<'_>]) -> Output {
        let mut t = Vec::new();
        for (n, ty, l, d) in entries {
            gnu(&mut t, n, *ty, l, d);
        }
        t.extend(std::iter::repeat_n(0u8, 1024));
        read_output(Bytes::from(t), 1 << 30).expect("reads")
    }

    fn names(o: &Output) -> Vec<String> {
        o.entries
            .iter()
            .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            .collect()
    }

    #[test]
    fn hostile_names_stay_data() {
        let o = read(&[
            (b"./", b'5', b"", b""),
            (b"./-rf", b'0', b"", b"a"),
            (b"./--checkpoint-action=exec=sh", b'0', b"", b"b"),
            (b"./a b", b'0', b"", b"c"),
            (b"./$(touch x)", b'0', b"", b"d"),
            (b"./new\nline", b'0', b"", b"e"),
            (b"./back\\slash", b'0', b"", b"f"),
        ]);
        assert_eq!(
            names(&o),
            [
                "$(touch x)",
                "--checkpoint-action=exec=sh",
                "-rf",
                "a b",
                "back\\slash",
                "new\nline"
            ]
        );
        assert!(o.dropped.is_empty());
    }

    #[test]
    fn links_out_of_the_tree_and_special_entries_are_dropped() {
        let o = read(&[
            (b"./node_modules/", b'5', b"", b""),
            (b"./node_modules/.bin/", b'5', b"", b""),
            (b"./node_modules/.bin/ms", b'2', b"../ms/bin.js", b""),
            (b"./escape", b'2', b"/etc/passwd", b""),
            (b"./up", b'2', b"../../etc", b""),
            (b"./deep/up", b'2', b"../../x", b""),
            (b"./hard", b'1', b"server.js", b""),
            (b"./fifo", b'6', b"", b""),
            (b"./dev", b'3', b"", b""),
            (b"../outside", b'0', b"", b"x"),
            (b"/abs", b'0', b"", b"x"),
            (b"./a//b", b'0', b"", b"x"),
        ]);
        assert_eq!(
            names(&o),
            ["node_modules", "node_modules/.bin", "node_modules/.bin/ms"]
        );
        let why: Vec<&str> = o.dropped.iter().map(|d| d.1).collect();
        assert_eq!(why.len(), 9, "{:?}", o.dropped);
    }

    #[test]
    fn an_entry_below_a_link_is_dropped() {
        let o = read(&[
            (b"./lib", b'2', b"node_modules", b""),
            (b"./lib/x.js", b'0', b"", b"x"),
        ]);
        assert_eq!(names(&o), ["lib"]);
    }

    #[test]
    fn long_names_read_and_modes_are_reduced() {
        let long = format!("./{}/{}", "d".repeat(120), "f".repeat(30));
        let mut t = Vec::new();
        gnu(&mut t, long.as_bytes(), b'0', b"", b"x");
        // A setuid, world-writable file.
        let mut h = [0u8; 512];
        h[..6].copy_from_slice(b"./suid");
        h[100..107].copy_from_slice(b"0006777");
        h[124..135].copy_from_slice(b"00000000000");
        h[156] = b'0';
        h[257..265].copy_from_slice(b"ustar  \0");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        t.extend_from_slice(&h);
        t.extend(std::iter::repeat_n(0u8, 1024));
        let o = read_output(Bytes::from(t), 1 << 30).expect("reads");
        // The file, the folder above it (added by the reader) and the setuid file.
        assert_eq!(o.entries.len(), 3);
        assert!(o.entries.iter().any(|e| e.path.len() == long.len() - 2));
        assert!(
            o.entries
                .iter()
                .any(|e| e.path == "d".repeat(120).into_bytes() && e.kind == Kind::Dir)
        );
        let suid = o.entries.iter().find(|e| e.path == b"suid").unwrap();
        assert_eq!(suid.mode, 0o755);
    }

    #[test]
    fn a_broken_tar_and_too_much_data_are_refused() {
        let mut t = Vec::new();
        gnu(&mut t, b"./a", b'0', b"", &[7u8; 2000]);
        let mut bad = t.clone();
        bad[0] ^= 1;
        assert!(read_output(Bytes::from(bad), 1 << 30).is_err());
        assert!(read_output(Bytes::from(t.clone()), 1000).is_err());
        let cut = Bytes::from(t[..1000].to_vec());
        assert!(read_output(cut, 1 << 30).is_err());
    }

    /// A stream cut after a whole entry (no end-of-archive marker) is refused: a cut attach
    /// connection must not become an image with files missing (review of R-2b).
    #[test]
    fn a_tar_cut_at_an_entry_boundary_is_refused() {
        let mut t = Vec::new();
        gnu(&mut t, b"./a", b'0', b"", b"aaa");
        gnu(&mut t, b"./b", b'0', b"", b"bbb");
        let whole = {
            let mut w = t.clone();
            w.extend(std::iter::repeat_n(0u8, 1024));
            w
        };
        assert_eq!(
            read_output(Bytes::from(whole.clone()), 1 << 30)
                .expect("a whole tar reads")
                .entries
                .len(),
            2
        );
        let e = read_output(Bytes::from(t.clone()), 1 << 30).expect_err("no end marker");
        assert!(e.contains("end-of-archive"), "{e}");
        // Cut after the first entry, exactly at the boundary.
        let e = read_output(Bytes::from(t[..1024].to_vec()), 1 << 30).expect_err("cut");
        assert!(e.contains("end-of-archive"), "{e}");
        // One zero block is a marker (some writers stop there); padding zeros are fine.
        let mut one = t.clone();
        one.extend(std::iter::repeat_n(0u8, 512));
        assert!(read_output(Bytes::from(one), 1 << 30).is_ok());
        let mut padded = whole.clone();
        padded.extend(std::iter::repeat_n(0u8, 8192));
        assert!(read_output(Bytes::from(padded), 1 << 30).is_ok());
        // A second archive after the marker (a script writing its own tar first) is refused.
        let mut twice = whole.clone();
        twice.extend_from_slice(&whole);
        let e = read_output(Bytes::from(twice), 1 << 30).expect_err("data after the marker");
        assert!(e.contains("after its end-of-archive"), "{e}");
    }

    /// Deep paths stay linear: a name over 4096 bytes is dropped, the folders above an entry are
    /// added once, and many distinct deep chains end the build instead of copying every parent
    /// (Kimi review of R-2b: a host-memory amplification).
    #[test]
    fn deep_paths_are_bounded() {
        // 40,000 segments in one name: dropped, nothing copied.
        let deep = "a/".repeat(40_000) + "f";
        let o = read(&[(deep.as_bytes(), b'0', b"", b"x")]);
        assert!(o.entries.is_empty());
        assert_eq!(o.dropped_total, 1);
        // A name just under the limit: its 2,000 folders are added, once each.
        let ok = "a/".repeat(2_000) + "f";
        let o = read(&[
            (ok.as_bytes(), b'0', b"", b"x"),
            ((ok.clone() + "2").as_bytes(), b'0', b"", b"y"),
        ]);
        assert_eq!(o.entries.len(), 2_000 + 2);
        assert!(
            o.entries.windows(2).all(|w| w[0].path < w[1].path),
            "sorted, no duplicate"
        );
        // Distinct deep chains: the sum of the names (with their folders) is bounded.
        let started = std::time::Instant::now();
        let mut t = Vec::new();
        for i in 0..64 {
            let name = format!("{i}/") + &"a/".repeat(2_000) + "f";
            gnu(&mut t, name.as_bytes(), b'0', b"", b"x");
        }
        t.extend(std::iter::repeat_n(0u8, 1024));
        let e = read_output(Bytes::from(t), 1 << 30).expect_err("over the name bound");
        assert!(e.contains("names are over"), "{e}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        // A link target over the limit is dropped.
        let o = read(&[(b"./l", b'2', "a/".repeat(2_100).as_bytes(), b"")]);
        assert!(o.entries.is_empty());
    }

    /// Many short paths whose folders are not in the tar: the folders the reader adds count
    /// against [`MAX_ENTRIES`], so a small output cannot become a layer of millions of folders
    /// (Kimi, run 12: 100,000 files with 20 missing folders each made 2.1 million entries).
    #[test]
    fn added_folders_count_against_the_entry_cap() {
        let mut t = Vec::new();
        // 25,000 files, each with 20 folders of its own: 525,000 names, under the name bound
        // (about 14 MiB) and the entry cap of the tar itself.
        for i in 0..25_000u32 {
            let name = format!("{i:05}/") + &"a/".repeat(19) + "f";
            gnu(&mut t, name.as_bytes(), b'0', b"", b"");
        }
        t.extend(std::iter::repeat_n(0u8, 1024));
        let e = read_output(Bytes::from(t), 1 << 30).expect_err("over the entry cap");
        assert!(e.contains("entries with the folders"), "{e}");
    }

    /// The names are counted as they are read, not after the whole tar is held (Kimi, run 12).
    #[test]
    fn the_name_bound_holds_while_reading() {
        // 20,000 distinct flat names of 4,000 bytes: 80 MB of names, no folder to add.
        let mut t = Vec::new();
        for i in 0..20_000u32 {
            let name = format!("{i:05}") + &"n".repeat(3_995);
            gnu(&mut t, name.as_bytes(), b'0', b"", b"");
        }
        t.extend(std::iter::repeat_n(0u8, 1024));
        // The error comes from the reading, before the map holds every name (a reader that
        // counts only in `complete`, with the folders, holds them all first).
        let e = read_output(Bytes::from(t), 1 << 30).expect_err("over the name bound");
        assert!(e.contains("as read"), "{e}");
        // One long-name record far over the limit is kept only up to the limit (and dropped).
        let huge = vec![b'x'; 1 << 20];
        assert_eq!(long_value(&huge).len(), MAX_NAME_BYTES + 1);
        let o = read(&[(&huge[..], b'0', b"", b"x")]);
        assert!(o.entries.is_empty());
        assert_eq!(o.dropped_total, 1);
        assert!(o.dropped[0].0.chars().count() <= 200);
    }

    /// A long-name record at the end of the tar (its entry missing) and a malformed PAX record
    /// refuse the output (Kimi, run 12): neither may become an image with a file missing or
    /// named from fallback metadata.
    #[test]
    fn a_dangling_or_malformed_extension_is_refused() {
        let mut t = Vec::new();
        gnu(&mut t, b"./a", b'0', b"", b"aaa");
        // Only the long-name record of the next entry: its own header is missing.
        let long = format!("./{}", "b".repeat(150));
        let mut next = Vec::new();
        gnu(&mut next, long.as_bytes(), b'0', b"", b"");
        t.extend_from_slice(&next[..1024]);
        t.extend(std::iter::repeat_n(0u8, 1024));
        let e = read_output(Bytes::from(t), 1 << 30).expect_err("dangling long name");
        assert!(e.contains("without its entry"), "{e}");

        let pax_tar = |records: &[u8]| {
            let mut t = Vec::new();
            gnu(&mut t, b"PaxHeader", b'x', b"", records);
            gnu(&mut t, b"./fallback", b'0', b"", b"x");
            t.extend(std::iter::repeat_n(0u8, 1024));
            Bytes::from(t)
        };
        let o = read_output(pax_tar(b"15 path=./real\n"), 1 << 30).expect("a valid record");
        assert_eq!(names(&o), ["real"]);
        for bad in [
            &b"15 path=./real"[..],   // no newline: the length is past the end
            &b"14 path=./real\n"[..], // a length that cuts the record
            &b"x5 path=./real\n"[..], // not a number
            &b"15 path:./real\n"[..], // no '='
        ] {
            let e = read_output(pax_tar(bad), 1 << 30).expect_err("malformed PAX");
            assert!(e.contains("malformed PAX"), "{e}");
        }
    }

    #[test]
    fn dropped_entries_are_counted_past_the_shown_ones() {
        let names: Vec<String> = (0..MAX_DROPPED_SHOWN + 5)
            .map(|i| format!("./f{i}"))
            .collect();
        let specs: Vec<Spec<'_>> = names
            .iter()
            .map(|n| (n.as_bytes(), b'6', &b""[..], &b""[..]))
            .collect();
        let o = read(&specs);
        assert_eq!(o.dropped.len(), MAX_DROPPED_SHOWN);
        assert_eq!(o.dropped_total, MAX_DROPPED_SHOWN + 5);
    }
}
