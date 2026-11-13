//! Deployment archives for the app cases, written by hand (the test kit has no archive
//! library, and a new dependency needs the person, design 18.4): `.tar.gz` with stored deflate
//! blocks, `.zip` with stored entries, and the hostile archives of card W-2 (zip slip, tar slip,
//! links, device files, archive bombs and entry-count bombs).
//!
//! The formats: ustar (POSIX.1-1988), gzip (RFC 1952), deflate (RFC 1951), zip (APPNOTE 6.3,
//! stored and deflated entries). `apps_meta.rs` has an ignored test that writes every archive to a
//! folder, so a person can check them with `tar`, `gzip -t` and `unzip -l`.

use std::sync::OnceLock;

/// The CRC-32 table (IEEE 802.3, reflected, polynomial 0xEDB88320).
fn crc_table() -> &'static [u32; 256] {
    static T: OnceLock<[u32; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    })
}

/// Feeds `data` into a CRC-32 register (not inverted at either end).
fn crc_update(mut reg: u32, data: &[u8]) -> u32 {
    let t = crc_table();
    for &b in data {
        reg = t[((reg ^ b as u32) & 0xff) as usize] ^ (reg >> 8);
    }
    reg
}

/// The CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    !crc_update(!0, data)
}

/// The CRC-32 of `prefix` followed by `n` zero bytes, without making the zeros.
pub fn crc32_then_zeros(prefix: &[u8], n: u64) -> u32 {
    !crc_zeros(crc_update(!0, prefix), n)
}

/// A 32x32 matrix over GF(2): column `i` is the image of bit `i`.
type Gf2 = [u32; 32];

fn gf2_apply(m: &Gf2, mut v: u32) -> u32 {
    let mut out = 0;
    let mut i = 0;
    while v != 0 {
        if v & 1 != 0 {
            out ^= m[i];
        }
        v >>= 1;
        i += 1;
    }
    out
}

fn gf2_mul(a: &Gf2, b: &Gf2) -> Gf2 {
    let mut out = [0u32; 32];
    for (i, col) in b.iter().enumerate() {
        out[i] = gf2_apply(a, *col);
    }
    out
}

/// Feeds `n` zero bytes into a CRC-32 register, in O(log n): one zero byte is a linear map of
/// the register, raised to the power `n` by squaring.
fn crc_zeros(reg: u32, mut n: u64) -> u32 {
    let t = crc_table();
    let mut step: Gf2 = [0; 32];
    for (i, col) in step.iter_mut().enumerate() {
        let r = 1u32 << i;
        *col = t[(r & 0xff) as usize] ^ (r >> 8);
    }
    let mut acc: Gf2 = [0; 32];
    for (i, col) in acc.iter_mut().enumerate() {
        *col = 1 << i;
    }
    while n > 0 {
        if n & 1 != 0 {
            acc = gf2_mul(&step, &acc);
        }
        step = gf2_mul(&step, &step);
        n >>= 1;
    }
    gf2_apply(&acc, reg)
}

/// The kind of a tar entry (the ustar `typeflag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TarKind {
    File,
    HardLink,
    SymLink,
    CharDevice,
    BlockDevice,
    Fifo,
}

impl TarKind {
    fn flag(self) -> u8 {
        match self {
            TarKind::File => b'0',
            TarKind::HardLink => b'1',
            TarKind::SymLink => b'2',
            TarKind::CharDevice => b'3',
            TarKind::BlockDevice => b'4',
            TarKind::Fifo => b'6',
        }
    }
}

fn put(field: &mut [u8], value: &[u8]) {
    let n = value.len().min(field.len());
    field[..n].copy_from_slice(&value[..n]);
}

fn octal(field: &mut [u8], value: u64) {
    // `width - 1` octal digits and a NUL.
    let digits = field.len() - 1;
    let s = format!("{value:0digits$o}");
    assert!(s.len() == digits, "{value} does not fit a tar field");
    put(field, s.as_bytes());
}

/// One ustar header block for an entry of `size` bytes.
pub fn tar_header(name: &str, kind: TarKind, size: u64, link: &str) -> [u8; 512] {
    assert!(
        name.len() <= 100 && link.len() <= 100,
        "tar names are short here"
    );
    let mut h = [0u8; 512];
    put(&mut h[0..100], name.as_bytes());
    let mode = match kind {
        TarKind::SymLink => 0o777,
        _ => 0o644,
    };
    octal(&mut h[100..108], mode);
    octal(&mut h[108..116], 1000);
    octal(&mut h[116..124], 1000);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], 1_700_000_000);
    h[156] = kind.flag();
    put(&mut h[157..257], link.as_bytes());
    put(&mut h[257..263], b"ustar\0");
    put(&mut h[263..265], b"00");
    put(&mut h[265..297], b"ruum");
    put(&mut h[297..329], b"ruum");
    if matches!(kind, TarKind::CharDevice | TarKind::BlockDevice) {
        // /dev/null (1, 3) and /dev/sda (8, 0).
        let (major, minor) = if kind == TarKind::CharDevice {
            (1, 3)
        } else {
            (8, 0)
        };
        octal(&mut h[329..337], major);
        octal(&mut h[337..345], minor);
    }
    // The checksum counts its own field as spaces.
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    put(&mut h[148..156], format!("{sum:06o}\0 ").as_bytes());
    h
}

/// One entry of a tar archive: a header and, for files, the data padded to 512 bytes.
pub struct TarEntry<'a> {
    pub name: &'a str,
    pub kind: TarKind,
    pub data: &'a [u8],
    pub link: &'a str,
}

impl<'a> TarEntry<'a> {
    pub fn file(name: &'a str, data: &'a [u8]) -> Self {
        Self {
            name,
            kind: TarKind::File,
            data,
            link: "",
        }
    }

    pub fn special(name: &'a str, kind: TarKind, link: &'a str) -> Self {
        Self {
            name,
            kind,
            data: &[],
            link,
        }
    }
}

/// The bytes of one entry.
fn tar_entry_bytes(e: &TarEntry) -> Vec<u8> {
    let mut out = tar_header(e.name, e.kind, e.data.len() as u64, e.link).to_vec();
    out.extend_from_slice(e.data);
    let pad = (512 - e.data.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

/// A tar archive (with the two zero blocks at the end).
pub fn tar(entries: &[TarEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend(tar_entry_bytes(e));
    }
    out.extend(std::iter::repeat_n(0u8, 1024));
    out
}

const GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];

/// Stored deflate blocks of `data` (at most 65535 bytes each); the last one is final when
/// `last` is true.
fn stored_blocks(out: &mut Vec<u8>, data: &[u8], last: bool) {
    let mut chunks = data.chunks(65_535).peekable();
    if data.is_empty() {
        out.push(u8::from(last));
        out.extend_from_slice(&[0, 0, 0xff, 0xff]);
        return;
    }
    while let Some(c) = chunks.next() {
        let fin = last && chunks.peek().is_none();
        out.push(u8::from(fin));
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
}

/// `data` in a gzip member with stored (not compressed) deflate blocks.
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = GZIP_HEADER.to_vec();
    stored_blocks(&mut out, data, true);
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// A `.tar.gz` of `files` (path, content).
pub fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let entries: Vec<TarEntry> = files.iter().map(|(n, d)| TarEntry::file(n, d)).collect();
    gzip(&tar(&entries))
}

/// One entry of a zip archive (stored). `unix_mode` sets the external attributes, for example
/// `0o120777` for a symbolic link (the data is then the link target).
pub struct ZipEntry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    pub unix_mode: Option<u32>,
}

impl<'a> ZipEntry<'a> {
    pub fn file(name: &'a str, data: &'a [u8]) -> Self {
        Self {
            name,
            data,
            unix_mode: None,
        }
    }
}

/// A zip archive with stored entries.
pub fn zip(entries: &[ZipEntry]) -> Vec<u8> {
    let records: Vec<ZipRecord> = entries
        .iter()
        .map(|e| ZipRecord {
            name: e.name,
            method: 0,
            crc: crc32(e.data),
            size: e.data.len() as u32,
            data: e.data.to_vec(),
            unix_mode: e.unix_mode,
        })
        .collect();
    zip_records(&records)
}

/// One zip entry as it is written: `data` is the stored or compressed data, `size` the size
/// after expansion and `crc` the CRC-32 of the expanded data.
struct ZipRecord<'a> {
    name: &'a str,
    /// 0 stored, 8 deflate.
    method: u16,
    crc: u32,
    size: u32,
    data: Vec<u8>,
    unix_mode: Option<u32>,
}

fn zip_records(entries: &[ZipRecord]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let offset = out.len() as u32;
        let csize = e.data.len() as u32;
        let name = e.name.as_bytes();
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&e.method.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0x5b21u16.to_le_bytes()); // date (2025-09-01)
        out.extend_from_slice(&e.crc.to_le_bytes());
        out.extend_from_slice(&csize.to_le_bytes());
        out.extend_from_slice(&e.size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra length
        out.extend_from_slice(name);
        out.extend_from_slice(&e.data);
        // Central directory header.
        let (made_by, attrs) = match e.unix_mode {
            Some(mode) => ((3u16 << 8) | 20, mode << 16),
            None => (20u16, 0u32),
        };
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&made_by.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&e.method.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x5b21u16.to_le_bytes());
        central.extend_from_slice(&e.crc.to_le_bytes());
        central.extend_from_slice(&csize.to_le_bytes());
        central.extend_from_slice(&e.size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&attrs.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend(central);
    let n = u16::try_from(entries.len()).expect("a zip here has at most 65535 entries");
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Writes bits least significant first, as deflate does.
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn new(out: Vec<u8>) -> Self {
        Self { out, acc: 0, n: 0 }
    }

    fn put(&mut self, value: u32, count: u32) {
        self.acc |= value << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn zeros(&mut self, mut count: u64) {
        while count > 0 && self.n != 0 {
            self.put(0, 1);
            count -= 1;
        }
        let bytes = (count / 8) as usize;
        self.out.resize(self.out.len() + bytes, 0);
        for _ in 0..count % 8 {
            self.put(0, 1);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// How many bytes the file of the archive bomb claims and expands to: 4 GiB, far over any
/// plan's archive limit, from about 4 MiB of gzip.
pub const BOMB_BYTES: u64 = 4 << 30;

/// How many bytes the deflated entry of the zip bomb expands to: 2 GiB (a zip without zip64
/// holds sizes below 4 GiB), from about 2 MiB.
pub const ZIP_BOMB_BYTES: u32 = 1 << 31;

/// How many files the entry-count bombs hold: each archive is small (a few MiB for the zip,
/// about 32 MiB for the stored `.tar.gz`, under every plan's archive limit) but has more files
/// than a static site needs. The contract names no number yet (see the pending entry of
/// `entry_count_bomb_is_refused`).
pub const COUNT_BOMB_FILES: usize = 65_000;

/// Writes one final dynamic Huffman block of `zeros` zero bytes. When `after_zero` is false,
/// the stream has no zero byte before the block (a fresh stream), so the block starts with one
/// zero literal before the first back reference of distance 1.
///
/// The block has three literal/length codes (literal `0`: `10`, end of block: `11`, length
/// 258: `0`) and two distance codes (distance 1: `0`), so each run of 258 zero bytes costs 2
/// bits (deflate's best ratio, about 1:1032).
fn zeros_block(b: &mut Bits, mut zeros: u64, after_zero: bool) {
    b.put(1, 1); // BFINAL
    b.put(2, 2); // BTYPE = dynamic
    b.put(286 - 257, 5); // HLIT
    b.put(2 - 1, 5); // HDIST
    b.put(18 - 4, 4); // HCLEN
    // Code lengths of the code length alphabet, in the order of RFC 1951 3.2.7:
    // 16 17 18 0 8 7 9 6 10 5 11 4 12 3 13 2 14 1. Symbol 18 gets 1 bit, 1 and 2 get 2 bits.
    for len in [0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 2] {
        b.put(len, 3);
    }
    // Canonical codes of that alphabet: 18 = `0`, 1 = `10`, 2 = `11` (written reversed).
    let cl_1 = |b: &mut Bits| b.put(0b01, 2);
    let cl_2 = |b: &mut Bits| b.put(0b11, 2);
    let cl_zeros = |b: &mut Bits, n: u32| {
        b.put(0, 1); // symbol 18
        b.put(n - 11, 7);
    };
    // Literal/length lengths: 0 -> 2, 1..=255 -> 0, 256 -> 2, 257..=284 -> 0, 285 -> 1.
    cl_2(b);
    cl_zeros(b, 138);
    cl_zeros(b, 117);
    cl_2(b);
    cl_zeros(b, 28);
    cl_1(b);
    // Distance lengths: 0 -> 1, 1 -> 1.
    cl_1(b);
    cl_1(b);
    if !after_zero && zeros > 0 {
        b.put(0b01, 2); // one zero literal, so distance 1 is valid
        zeros -= 1;
    }
    // The data: runs of 258 (code `0`, distance code `0`), then single zero literals (`10`),
    // then the end of the block (`11`).
    let runs = zeros / 258;
    let rest = zeros % 258;
    b.zeros(runs * 2);
    for _ in 0..rest {
        b.put(0b01, 2);
    }
    b.put(0b11, 2);
}

/// An archive bomb: a valid `.tar.gz` with `app.yaml` (`runtime: static`) and one file of
/// [`BOMB_BYTES`] zero bytes, about 4 MiB compressed.
///
/// The deflate stream: the tar headers in stored blocks, then one dynamic Huffman block of
/// zeros ([`zeros_block`]). The byte before the first run is a zero of the tar header, so
/// distance 1 is valid.
pub fn bomb_tar_gz() -> Vec<u8> {
    let manifest = b"runtime: static\n";
    let mut prefix = tar_entry_bytes(&TarEntry::file("app.yaml", manifest));
    prefix.extend_from_slice(&tar_header("zeros.bin", TarKind::File, BOMB_BYTES, ""));
    // The file (a multiple of 512 bytes, so no padding) and the two end blocks.
    let zeros: u64 = BOMB_BYTES + 1024;

    let mut out = GZIP_HEADER.to_vec();
    stored_blocks(&mut out, &prefix, false);
    let mut b = Bits::new(out);
    zeros_block(&mut b, zeros, true);
    let mut out = b.finish();

    out.extend_from_slice(&crc32_then_zeros(&prefix, zeros).to_le_bytes());
    out.extend_from_slice(&((prefix.len() as u64 + zeros) as u32).to_le_bytes());
    out
}

/// A zip bomb: a valid `.zip` with `app.yaml` (stored) and `zeros.bin`, a deflated entry of
/// about 2 MiB that expands to [`ZIP_BOMB_BYTES`] zero bytes. Its sizes and CRC are true, so
/// a reader that trusts the declared size and one that counts while it expands both see 2 GiB.
pub fn bomb_zip() -> Vec<u8> {
    let manifest = b"runtime: static\n";
    let mut b = Bits::new(Vec::new());
    zeros_block(&mut b, u64::from(ZIP_BOMB_BYTES), false);
    let deflated = b.finish();
    zip_records(&[
        ZipRecord {
            name: "app.yaml",
            method: 0,
            crc: crc32(manifest),
            size: manifest.len() as u32,
            data: manifest.to_vec(),
            unix_mode: None,
        },
        ZipRecord {
            name: "zeros.bin",
            method: 8,
            crc: crc32_then_zeros(&[], u64::from(ZIP_BOMB_BYTES)),
            size: ZIP_BOMB_BYTES,
            data: deflated,
            unix_mode: None,
        },
    ])
}

/// The names of the empty files of the entry-count bombs.
pub fn count_bomb_names() -> Vec<String> {
    (0..COUNT_BOMB_FILES)
        .map(|i| format!("f/{i:05}.txt"))
        .collect()
}

/// An entry-count bomb as a `.zip`: `app.yaml` and [`COUNT_BOMB_FILES`] empty files.
pub fn count_bomb_zip() -> Vec<u8> {
    let names = count_bomb_names();
    let mut entries = vec![ZipEntry::file("app.yaml", b"runtime: static\n")];
    entries.extend(names.iter().map(|n| ZipEntry::file(n, b"")));
    zip(&entries)
}

/// An entry-count bomb as a `.tar.gz`: `app.yaml` and [`COUNT_BOMB_FILES`] empty files.
pub fn count_bomb_tar_gz() -> Vec<u8> {
    let names = count_bomb_names();
    let mut entries = vec![TarEntry::file("app.yaml", b"runtime: static\n")];
    entries.extend(names.iter().map(|n| TarEntry::file(n, b"")));
    gzip(&tar(&entries))
}

/// The zip entries that escape the archive, with the file name of each archive: `..`
/// segments, an absolute path, the Windows forms with `\` (APPNOTE 4.4.17 allows only `/`), drive
/// paths and UNC paths (decision 0006, section H).
pub const ZIP_ESCAPES: &[(&str, &str)] = &[
    ("slip.zip", "../../outside.html"),
    ("abs.zip", "/etc/ruum.html"),
    ("backslash.zip", "..\\..\\outside.html"),
    ("deep-backslash.zip", "dist\\..\\..\\outside.html"),
    ("drive.zip", "C:\\ruum\\outside.html"),
    ("drive-slash.zip", "C:/ruum/outside.html"),
    ("unc.zip", "\\\\server\\share\\outside.html"),
    ("unc-slash.zip", "//server/share/outside.html"),
];

/// The tar entries that escape the archive, with the file name of each archive.
pub const TAR_ESCAPES: &[(&str, &str)] = &[
    ("slip.tar.gz", "../outside.html"),
    ("deep.tar.gz", "dist/../../outside.html"),
    ("abs.tar.gz", "/etc/ruum.html"),
    ("backslash.tar.gz", "..\\outside.html"),
    ("deep-backslash.tar.gz", "dist\\..\\..\\outside.html"),
    ("drive.tar.gz", "C:\\ruum\\outside.html"),
    ("unc.tar.gz", "\\\\server\\share\\outside.html"),
];
