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
//! - **refused** (the build fails): a broken tar (bad checksum, an entry past the end).
//!
//! A later entry of the same name replaces an earlier one (tar semantics).

use bytes::Bytes;
use std::collections::BTreeMap;

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

/// The kept entries (sorted by path) and the dropped ones with the reason.
#[derive(Debug, Default)]
pub struct Output {
    pub entries: Vec<OutEntry>,
    pub dropped: Vec<(String, &'static str)>,
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

/// PAX records (`path` and `linkpath` only).
fn pax(data: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let (mut path, mut link) = (None, None);
    let mut rest = data;
    while !rest.is_empty() {
        let Some(sp) = rest.iter().position(|b| *b == b' ') else {
            break;
        };
        let Some(len) = std::str::from_utf8(&rest[..sp])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            break;
        };
        if len <= sp + 1 || len > rest.len() {
            break;
        }
        let rec = &rest[sp + 1..len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(eq) = rec.iter().position(|b| *b == b'=') {
            match &rec[..eq] {
                b"path" => path = Some(rec[eq + 1..].to_vec()),
                b"linkpath" => link = Some(rec[eq + 1..].to_vec()),
                _ => {}
            }
        }
        rest = &rest[len..];
    }
    (path, link)
}

/// The checked relative form of a name, or why it is dropped. `Ok(None)`: the root itself.
fn clean(name: &[u8]) -> Result<Option<Vec<u8>>, &'static str> {
    if name.contains(&0) {
        return Err("a NUL byte in the name");
    }
    if name.starts_with(b"/") {
        return Err("an absolute name");
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
    if target.is_empty() || target.starts_with(b"/") || target.contains(&0) {
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
    let mut dropped = Vec::new();
    let mut long_name: Option<Vec<u8>> = None;
    let mut long_link: Option<Vec<u8>> = None;
    let mut total: u64 = 0;
    loop {
        if at + 512 > tar.len() {
            // A tar that ends without its end blocks: what was read is complete entries.
            if at == tar.len() {
                break;
            }
            return Err("the output ends in the middle of a header".to_owned());
        }
        let h = &tar[at..at + 512];
        if h.iter().all(|b| *b == 0) {
            break;
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
                long_name = Some(cstr(&data).to_vec());
                continue;
            }
            b'K' => {
                long_link = Some(cstr(&data).to_vec());
                continue;
            }
            b'x' => {
                let (p, l) = pax(&data);
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
                dropped.push((shown(&name), why));
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
                    dropped.push((shown(&path), "a link that leaves the output"));
                    continue;
                }
                Kind::Symlink(link)
            }
            b'1' => {
                dropped.push((shown(&path), "a hard link"));
                continue;
            }
            b'3' | b'4' => {
                dropped.push((shown(&path), "a device"));
                continue;
            }
            b'6' => {
                dropped.push((shown(&path), "a FIFO"));
                continue;
            }
            _ => {
                dropped.push((shown(&path), "an unknown entry type"));
                continue;
            }
        };
        let mode = if matches!(kind, Kind::Dir) {
            0o755
        } else {
            mode | 0o400
        };
        kept.insert(path.clone(), OutEntry { path, mode, kind });
    }
    // An entry below a non-folder (a file or a link named like a folder) would write through
    // it at unpack time: drop it.
    let names: Vec<Vec<u8>> = kept.keys().cloned().collect();
    for n in names {
        let mut bad = false;
        for (i, b) in n.iter().enumerate() {
            if *b == b'/'
                && kept
                    .get(&n[..i])
                    .is_some_and(|p| !matches!(p.kind, Kind::Dir))
            {
                bad = true;
                break;
            }
        }
        if bad {
            kept.remove(&n);
            dropped.push((shown(&n), "an entry below a file or a link"));
        }
    }
    Ok(Output {
        entries: kept.into_values().collect(),
        dropped,
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
        assert_eq!(o.entries.len(), 2);
        assert!(o.entries.iter().any(|e| e.path.len() == long.len() - 2));
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
}
