//! The unpacker against every hostile form of card W-2s.

use super::*;
use flate2::Compression;
use std::io::Write;

fn limits() -> Limits {
    Limits {
        max_total_bytes: 50 * 1024 * 1024,
        max_entry_bytes: 32 * 1024 * 1024,
        max_files: 100,
        max_ratio: 200,
    }
}

fn header(name: &[u8], flag: u8, size: u64, link: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    put_octal(&mut h[100..108], 0o644);
    put_octal(&mut h[108..116], 0);
    put_octal(&mut h[116..124], 0);
    put_octal(&mut h[124..136], size);
    put_octal(&mut h[136..148], 0);
    h[156] = flag;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

/// A tar of raw entries `(name, flag, data, link)`.
fn tar(entries: &[(&[u8], u8, &[u8], &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, flag, data, link) in entries {
        out.extend_from_slice(&header(name, *flag, data.len() as u64, link));
        out.extend_from_slice(data);
        out.extend(std::iter::repeat_n(
            0u8,
            padding(data.len() as u64) as usize,
        ));
    }
    out.extend(std::iter::repeat_n(0u8, 1024));
    out
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn crc(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

/// One zip entry as written; every field can lie.
#[derive(Clone)]
struct Z {
    name: Vec<u8>,
    local_name: Option<Vec<u8>>,
    method: u16,
    data: Vec<u8>,
    crc: u32,
    usize: u32,
    local_usize: Option<u32>,
    mode: Option<u32>,
}

impl Z {
    fn stored(name: &str, data: &[u8]) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            local_name: None,
            method: 0,
            data: data.to_vec(),
            crc: crc(data),
            usize: data.len() as u32,
            local_usize: None,
            mode: None,
        }
    }

    fn deflated(name: &str, data: &[u8]) -> Self {
        Self {
            method: 8,
            data: deflate(data),
            ..Self::stored(name, data)
        }
    }
}

fn zip(entries: &[Z]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let offset = out.len() as u32;
        let lname = e.local_name.clone().unwrap_or_else(|| e.name.clone());
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&e.method.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&e.crc.to_le_bytes());
        out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&e.local_usize.unwrap_or(e.usize).to_le_bytes());
        out.extend_from_slice(&(lname.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&lname);
        out.extend_from_slice(&e.data);
        let (made_by, attrs) = match e.mode {
            Some(m) => ((3u16 << 8) | 20, m << 16),
            None => (20, 0),
        };
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&made_by.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&e.method.to_le_bytes());
        central.extend_from_slice(&[0, 0, 0, 0]);
        central.extend_from_slice(&e.crc.to_le_bytes());
        central.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&e.usize.to_le_bytes());
        central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&attrs.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(&e.name);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend(central);
    let n = entries.len() as u16;
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

const MANIFEST: &[u8] = b"runtime: static\n";

fn hostile(r: R<Unpacked>, what: &str) {
    match r {
        Err(Refusal::Hostile { .. }) => {}
        other => panic!("{what}: expected a hostile refusal, got {other:?}"),
    }
}

fn limited(r: R<Unpacked>, what: &str) {
    match r {
        Err(Refusal::Limit { .. }) => {}
        other => panic!("{what}: expected a limit refusal, got {other:?}"),
    }
}

#[test]
fn good_archives_unpack() {
    let t = tar(&[
        (b"./", b'5', b"", ""),
        (b"./app.yaml", b'0', MANIFEST, ""),
        (b"assets/", b'5', b"", ""),
        (b"assets/a.js", b'0', b"x()", ""),
    ]);
    for bytes in [t.clone(), gz(&t)] {
        let u = unpack(&bytes, &limits()).unwrap();
        assert_eq!(u.names(), ["app.yaml", "assets/a.js"]);
        assert_eq!(&u.get("app.yaml").unwrap().data[..], MANIFEST);
    }
    let z = zip(&[
        Z::stored("app.yaml", MANIFEST),
        Z::stored("dist/", b""),
        Z::deflated("dist/index.html", &b"<h1>hi</h1>".repeat(50)),
    ]);
    let u = unpack(&z, &limits()).unwrap();
    assert_eq!(u.names(), ["app.yaml", "dist/index.html"]);
    assert_eq!(u.total_bytes(), MANIFEST.len() as u64 + 550);
    assert!(matches!(
        unpack(b"not an archive at all", &limits()),
        Err(Refusal::Corrupt { .. })
    ));
}

#[test]
fn hostile_names_are_refused() {
    for bad in [
        "../outside.html",
        "dist/../../outside.html",
        "/etc/ruum.html",
        "..\\outside.html",
        "dist\\..\\..\\outside.html",
        "C:\\ruum\\outside.html",
        "C:/ruum/outside.html",
        "\\\\server\\share\\outside.html",
        "//server/share/outside.html",
        "a//b",
        "a/./b",
        "a\nb",
        "a\u{0}b",
        "",
        "..",
    ] {
        assert!(check_name(bad).is_err(), "{bad:?}");
        let t = gz(&tar(&[
            (b"app.yaml", b'0', MANIFEST, ""),
            (bad.as_bytes(), b'0', b"x", ""),
        ]));
        if !bad.is_empty() {
            // A tar name ends at its first NUL (as every tar reader reads it).
            if !bad.contains('\0') {
                hostile(unpack(&t, &limits()), bad);
            }
            let z = zip(&[Z::stored("app.yaml", MANIFEST), Z::stored(bad, b"x")]);
            hostile(unpack(&z, &limits()), bad);
        }
    }
    assert_eq!(check_name("./a/b.js").unwrap().as_deref(), Some("a/b.js"));
    assert_eq!(check_name("dir/").unwrap(), None);
}

#[test]
fn links_devices_and_fifos_are_refused() {
    for (flag, link) in [
        (b'1', "../../secret"),
        (b'2', "/etc/passwd"),
        (b'3', ""),
        (b'4', ""),
        (b'6', ""),
        (b'K', ""),
        (b'S', ""),
    ] {
        let t = gz(&tar(&[
            (b"app.yaml", b'0', MANIFEST, ""),
            (b"x", flag, b"", link),
        ]));
        hostile(unpack(&t, &limits()), &format!("flag {}", flag as char));
    }
    for mode in [0o120_777, 0o020_644, 0o060_644, 0o010_644, 0o140_644] {
        let mut e = Z::stored("index.html", b"/etc/passwd");
        e.mode = Some(mode);
        let z = zip(&[Z::stored("app.yaml", MANIFEST), e]);
        hostile(unpack(&z, &limits()), &format!("mode {mode:o}"));
    }
    // A regular unix mode is fine.
    let mut e = Z::stored("index.html", b"hi");
    e.mode = Some(0o100_644);
    assert!(unpack(&zip(&[e]), &limits()).is_ok());
}

#[test]
fn a_name_twice_is_refused() {
    let t = gz(&tar(&[
        (b"app.yaml", b'0', MANIFEST, ""),
        (b"./app.yaml", b'0', b"runtime: node22\n", ""),
    ]));
    hostile(unpack(&t, &limits()), "app.yaml twice in a tar");
    let z = zip(&[Z::stored("app.yaml", MANIFEST), Z::stored("app.yaml", b"x")]);
    hostile(unpack(&z, &limits()), "app.yaml twice in a zip");
}

#[test]
fn tar_extensions_that_escape_are_refused() {
    let long = b"../../../outside.html";
    let t = gz(&tar(&[
        (b"app.yaml", b'0', MANIFEST, ""),
        (b"././@LongLink", b'L', long, ""),
        (b"harmless.html", b'0', b"x", ""),
    ]));
    hostile(unpack(&t, &limits()), "a GNU long name that escapes");
    let rec = |k: &str, v: &str| {
        let body = format!(" {k}={v}\n");
        let mut n = body.len() + 1;
        while format!("{n}{body}").len() != n {
            n += 1;
        }
        format!("{n}{body}")
    };
    let pax = rec("path", "/etc/cron.d/x");
    let t = gz(&tar(&[
        (b"app.yaml", b'0', MANIFEST, ""),
        (b"PaxHeader", b'x', pax.as_bytes(), ""),
        (b"harmless.html", b'0', b"x", ""),
    ]));
    hostile(unpack(&t, &limits()), "a pax path that escapes");
    let pax = rec("size", "10");
    let t = gz(&tar(&[
        (b"PaxHeader", b'x', pax.as_bytes(), ""),
        (b"a", b'0', b"x", ""),
    ]));
    hostile(unpack(&t, &limits()), "a pax size");
    // A long name that is fine is used.
    let name = format!("{}/index.html", "d".repeat(120));
    let t = gz(&tar(&[
        (b"././@LongLink", b'L', name.as_bytes(), ""),
        (b"short", b'0', b"x", ""),
    ]));
    assert_eq!(unpack(&t, &limits()).unwrap().names(), [name.as_str()]);
}

#[test]
fn zip_local_and_central_must_agree() {
    let mut e = Z::stored("index.html", b"<h1>hi</h1>");
    e.local_name = Some(b"../evil.html".to_vec());
    hostile(unpack(&zip(&[e]), &limits()), "local name differs");
    let mut e = Z::stored("index.html", b"<h1>hi</h1>");
    e.local_usize = Some(3);
    hostile(unpack(&zip(&[e]), &limits()), "local size differs");
}

#[test]
fn zip64_and_size_lies_are_refused() {
    // A declared size smaller than the data (deflate that expands to more).
    let mut e = Z::deflated("big.txt", &vec![b'a'; 10_000]);
    e.usize = 100;
    e.crc = crc(&[b'a'; 100]);
    hostile(unpack(&zip(&[e]), &limits()), "a size lie");
    // A declared size larger than the data.
    let mut e = Z::stored("small.txt", b"abc");
    e.usize = 10;
    hostile(unpack(&zip(&[e]), &limits()), "a stored size lie");
    // zip64 markers in the sizes.
    let mut e = Z::stored("x.txt", b"abc");
    e.usize = 0xffff_ffff;
    e.local_usize = Some(0xffff_ffff);
    hostile(unpack(&zip(&[e]), &limits()), "a zip64 size");
    // A bad CRC.
    let mut e = Z::stored("x.txt", b"abc");
    e.crc ^= 1;
    hostile(unpack(&zip(&[e]), &limits()), "a bad crc");
    // A zip64 end record count.
    let mut z = zip(&[Z::stored("x.txt", b"abc")]);
    let n = z.len();
    z[n - 12..n - 10].copy_from_slice(&0xffffu16.to_le_bytes());
    assert!(unpack(&z, &limits()).is_err());
}

#[test]
fn overlapping_zip_entries_are_refused() {
    let mut z = zip(&[Z::stored("a.txt", b"abc"), Z::stored("b.txt", b"abc")]);
    // Point the second central entry at the first local header.
    let n = z.len();
    let cd_offset = u32::from_le_bytes(z[n - 6..n - 2].try_into().unwrap()) as usize;
    let first_len = 46 + 5;
    let off = cd_offset + first_len + 42;
    z[off..off + 4].copy_from_slice(&0u32.to_le_bytes());
    hostile(unpack(&z, &limits()), "overlap");
}

#[test]
fn bombs_and_counts_hit_the_limits() {
    // A ratio bomb inside one zip entry: 8 MiB of zeros deflate to a few KiB.
    let zeros = vec![0u8; 8 * 1024 * 1024];
    let z = zip(&[
        Z::stored("app.yaml", MANIFEST),
        Z::deflated("zeros.bin", &zeros),
    ]);
    limited(unpack(&z, &limits()), "a zip ratio bomb");
    // The same in a tar.gz (the ratio of the whole stream).
    let t = gz(&tar(&[
        (b"app.yaml", b'0', MANIFEST, ""),
        (b"zeros.bin", b'0', &zeros, ""),
    ]));
    limited(unpack(&t, &limits()), "a tar.gz ratio bomb");
    // A declared size over the entry limit is refused before reading.
    let mut t = tar(&[(b"app.yaml", b'0', MANIFEST, "")]);
    t.truncate(t.len() - 1024);
    t.extend_from_slice(&header(b"zeros.bin", b'0', 4 << 30, ""));
    limited(unpack(&gz(&t), &limits()), "a 4 GiB entry");
    // Too many files.
    let names: Vec<String> = (0..101).map(|i| format!("f/{i}.txt")).collect();
    let entries: Vec<(&[u8], u8, &[u8], &str)> = names
        .iter()
        .map(|n| (n.as_bytes(), b'0', &b""[..], ""))
        .collect();
    limited(unpack(&gz(&tar(&entries)), &limits()), "a tar count bomb");
    let zs: Vec<Z> = names.iter().map(|n| Z::stored(n, b"")).collect();
    limited(unpack(&zip(&zs), &limits()), "a zip count bomb");
    // Exactly the limit is fine.
    let ok: Vec<Z> = names.iter().take(100).map(|n| Z::stored(n, b"")).collect();
    assert!(unpack(&zip(&ok), &limits()).is_ok());
    // The whole archive over the plan's limit.
    let small = Limits {
        max_total_bytes: 100,
        ..limits()
    };
    limited(unpack(&[0u8; 101], &small), "archive bytes");
}

#[test]
fn packed_folders_unpack_again() {
    let deep = format!("{}/{}.html", "a".repeat(150), "b".repeat(90));
    let files = vec![
        ("app.yaml".to_string(), MANIFEST),
        ("index.html".to_string(), &b"<h1>x</h1>"[..]),
        (deep.clone(), &b"deep"[..]),
    ];
    let t = pack_tar(&files).unwrap();
    let u = unpack(&t, &limits()).unwrap();
    assert_eq!(u.names(), ["app.yaml", "index.html", deep.as_str()]);
    let too_long = vec![("x".repeat(300), &b""[..])];
    assert!(pack_tar(&too_long).is_err());
}

#[test]
fn refusals_map_to_the_codes_of_the_contract() {
    let h = Refusal::hostile("../x", "a .. segment").into_error();
    assert_eq!(h.http_status(), 422);
    assert_eq!(h.to_api().details.unwrap()["stage"], "unpack");
    let l = Refusal::Limit {
        what: "files",
        limit: 10,
    }
    .into_error();
    assert_eq!(l.http_status(), 413);
    assert_eq!(Refusal::corrupt("x").into_error().http_status(), 422);
}
