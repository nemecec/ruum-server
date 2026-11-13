//! Checks of the M1 app cases themselves (card T-4a): every case names its operations, every
//! operation is in the contract, a case over a planned operation is reported as planned, every
//! pending entry names its case and a card, each of the 12 M1 operations has a case, and the
//! hand-written archives are well formed (also as `gzip`, `tar` and Python's `zipfile` and
//! `tarfile` read them). It also checks, now, two rules that the planned cases rely on: the
//! manifest schema refuses paths that leave the archive, and the secret scan finds encoded
//! values.

mod common;

use common::apps::{self, PENDING, app_gate, pending_reason};
use common::apps_archive::{self, TarEntry, TarKind, ZipEntry};
use common::{Outcome, all_cases};
use ruum_testkit::contract;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The 12 operations of milestone M1 that card T-4a covers.
const M1_OPERATIONS: &[&str] = &[
    "createApp",
    "getApp",
    "updateAppSettings",
    "listAppDeployments",
    "getAppDeployment",
    "createAppDeployment",
    "publishAppDeployment",
    "restoreVersion",
    "listAppRoles",
    "listAppRoleGrants",
    "addAppRoleGrant",
    "removeAppRoleGrant",
];

#[test]
fn every_app_case_names_its_operations() {
    let ops: BTreeSet<String> = contract::operations().into_iter().map(|o| o.id).collect();
    let mut names: BTreeSet<&str> = all_cases().iter().map(|c| c.name).collect();
    for c in apps::all_cases() {
        assert!(names.insert(c.name), "two cases are named {}", c.name);
        assert!(!c.ops.is_empty(), "{} names no operation", c.name);
        assert!(!c.note.trim().is_empty(), "{} has no note", c.name);
        for op in c.ops {
            assert!(
                ops.contains(*op),
                "{} names {op}, which is not in the contract",
                c.name
            );
        }
        let unique: BTreeSet<&&str> = c.ops.iter().collect();
        assert_eq!(
            unique.len(),
            c.ops.len(),
            "{} names an operation twice",
            c.name
        );
    }
}

/// A case is planned while any of its operations is planned, also with
/// `RUUM_REQUIRE_IMPLEMENTED=1`; then pending when [`PENDING`] lists it.
#[test]
fn app_cases_are_planned_then_pending() {
    for c in apps::all_cases() {
        let planned: Vec<String> = c
            .ops
            .iter()
            .filter(|op| contract::is_planned(op))
            .map(|op| op.to_string())
            .collect();
        let g = app_gate(&c, contract::is_planned);
        if !planned.is_empty() {
            assert_eq!(g, Some(Outcome::Planned(planned)), "{}", c.name);
        } else if let Some(r) = pending_reason(c.name) {
            assert_eq!(g, Some(Outcome::Blocked(r)), "{}", c.name);
        } else {
            assert_eq!(g, None, "{}", c.name);
        }
    }
    // The gate with a made-up set of planned operations.
    let cases = apps::all_cases();
    let (name, reason) = PENDING[0];
    let blocked = cases
        .iter()
        .find(|c| c.name == name)
        .expect("a pending case");
    assert_eq!(app_gate(blocked, |_| false), Some(Outcome::Blocked(reason)));
    assert!(matches!(
        app_gate(blocked, |op| op == "createApp"),
        Some(Outcome::Planned(_))
    ));
}

/// Each pending entry names an app case and a card (`X-1`, `W-3b`, `D-3a`), and is not also
/// in the database list of `mod.rs`.
#[test]
fn app_pending_entries_name_a_case_and_a_card() {
    let names: BTreeSet<&str> = apps::all_cases().iter().map(|c| c.name).collect();
    let mut seen = BTreeSet::new();
    assert!(
        PENDING.len() <= 12,
        "the pending list grows: {}",
        PENDING.len()
    );
    for (case, reason) in PENDING {
        assert!(seen.insert(*case), "{case} is listed twice");
        assert!(
            names.contains(case),
            "the pending entry {case} names no app case"
        );
        assert!(
            common::pending_reason(case).is_none(),
            "{case} is also in the database pending list"
        );
        let has_card = reason
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .any(|w| {
                let mut parts = w.splitn(2, '-');
                matches!(
                    (parts.next(), parts.next()),
                    (Some(l), Some(n)) if l.len() == 1
                        && l.chars().all(|c| c.is_ascii_uppercase())
                        && n.chars().next().is_some_and(|c| c.is_ascii_digit())
                        && n.chars().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
                        && n.len() <= 3
                )
            });
        assert!(
            has_card,
            "the pending entry {case} names no card: {reason:?}"
        );
    }
}

/// Each M1 operation has a case of its own beyond the matrix, and the M1 operations that card
/// T-4a covers are in the contract.
#[test]
fn every_m1_operation_has_a_case() {
    let ops: BTreeSet<String> = contract::operations().into_iter().map(|o| o.id).collect();
    let named: BTreeSet<&str> = apps::all_cases()
        .iter()
        .filter(|c| pending_reason(c.name).is_none())
        .flat_map(|c| c.ops.iter().copied())
        .collect();
    for op in M1_OPERATIONS {
        assert!(ops.contains(*op), "{op} is not in the contract");
        assert!(named.contains(op), "{op} has no app case that runs");
    }
}

#[test]
fn crc32_is_right() {
    assert_eq!(apps_archive::crc32(b"123456789"), 0xCBF4_3926);
    for n in [0u64, 1, 7, 512, 1000, 65_536] {
        let data = [b"abc".as_slice(), &vec![0u8; n as usize]].concat();
        assert_eq!(
            apps_archive::crc32_then_zeros(b"abc", n),
            apps_archive::crc32(&data)
        );
    }
}

#[test]
fn tar_headers_are_ustar() {
    let h = apps_archive::tar_header("app.yaml", TarKind::File, 16, "");
    assert_eq!(&h[257..263], b"ustar\0");
    assert_eq!(&h[124..135], b"00000000020");
    // The checksum: the sum of the bytes with the field counted as spaces.
    let mut copy = h;
    copy[148..156].copy_from_slice(b"        ");
    let sum: u32 = copy.iter().map(|&b| b as u32).sum();
    let field = std::str::from_utf8(&h[148..154]).expect("octal");
    assert_eq!(u32::from_str_radix(field, 8).expect("octal"), sum);
    let big = apps_archive::tar_header("zeros.bin", TarKind::File, apps_archive::BOMB_BYTES, "");
    assert_eq!(&big[124..135], b"40000000000");
    let t = apps_archive::tar(&[
        TarEntry::file("a", b"x"),
        TarEntry::special("l", TarKind::SymLink, "/etc"),
    ]);
    assert_eq!(t.len(), 512 * 2 + 512 + 1024);
}

#[test]
fn gzip_and_zip_have_their_framing() {
    let g = apps_archive::gzip(b"hello");
    assert_eq!(&g[..3], &[0x1f, 0x8b, 8]);
    assert_eq!(g[10], 1, "one final stored block");
    assert_eq!(&g[g.len() - 4..], &5u32.to_le_bytes());
    assert_eq!(
        &g[g.len() - 8..g.len() - 4],
        &apps_archive::crc32(b"hello").to_le_bytes()
    );
    let z = apps_archive::zip(&[ZipEntry::file("app.yaml", b"runtime: static\n")]);
    assert_eq!(&z[..4], &[0x50, 0x4b, 3, 4]);
    let eocd = z.len() - 22;
    assert_eq!(&z[eocd..eocd + 4], &[0x50, 0x4b, 5, 6]);
}

#[test]
fn the_bomb_is_small_and_claims_four_gib() {
    let b = apps_archive::bomb_tar_gz();
    assert!(b.len() < 5 << 20, "the bomb is {} bytes", b.len());
    let isize = u32::from_le_bytes(b[b.len() - 4..].try_into().expect("4 bytes"));
    let total = (512 * 2 + 512) as u64 + apps_archive::BOMB_BYTES + 1024;
    assert_eq!(isize, total as u32);
}

#[test]
fn the_zip_bomb_is_small_and_claims_two_gib() {
    let b = apps_archive::bomb_zip();
    assert!(b.len() < 3 << 20, "the zip bomb is {} bytes", b.len());
    let z = apps_archive::count_bomb_zip();
    assert!(z.len() < 8 << 20, "the zip count bomb is {} bytes", z.len());
    let t = apps_archive::count_bomb_tar_gz();
    // Under the smallest plan's archive limit (free: 50 MiB), so only the file count refuses it.
    assert!(
        t.len() < 50 << 20,
        "the tar count bomb is {} bytes",
        t.len()
    );
    // Both go up in one `putContent` (backlog T-40): a bigger body would be refused by the
    // upload limit, and the case would not reach the unpacker at all.
    for (name, len) in [("zip", z.len()), ("tar.gz", t.len())] {
        assert!(
            len <= ruum_core::blobs::MAX_PUT_CONTENT_BYTES,
            "the {name} count bomb ({len} bytes) is over the putContent limit"
        );
    }
}

/// Every field of the manifest that names a path in the archive refuses a path that leaves
/// it (`archive_path` of `contracts/manifests/app.schema.json`, decision 0006 H), and still
/// takes ordinary paths. Runs now, through the server's own manifest check, although the
/// deployment cases that rely on it are planned.
#[test]
fn manifest_paths_stay_inside_the_archive() {
    use ruum_core::apps::{manifest, plan::AppPlanLimits};
    use ruum_core::tenants::Plan;
    let limits = AppPlanLimits::of(Plan::Team);
    type Make = fn(&str) -> Value;
    let fields: [(&str, Make); 6] = [
        (
            "/static/root",
            |v| json!({ "runtime": "static", "static": { "root": v } }),
        ),
        (
            "/static/spa_fallback",
            |v| json!({ "runtime": "static", "static": { "spa_fallback": v } }),
        ),
        (
            "/migrations/db",
            |v| json!({ "runtime": "static", "migrations": { "db": v } }),
        ),
        ("/apis/orders/spec", |v| {
            json!({ "runtime": "static", "apis": { "orders": {
                "visibility": "internal", "protocol": "http", "spec": v,
                "scopes": ["orders.read"],
            } } })
        }),
        (
            "/telemetry/privacy_file",
            |v| json!({ "runtime": "static", "telemetry": { "privacy_file": v } }),
        ),
        (
            "/telemetry/source_maps",
            |v| json!({ "runtime": "static", "telemetry": { "source_maps": v } }),
        ),
    ];
    let good = [
        "dist",
        "./dist",
        "dist/",
        "migrations/",
        "a/b/index.html",
        ".well-known/x",
        "..a",
        "...",
        "a//b",
    ];
    let bad = [
        "..",
        "../x",
        "../../outside",
        "a/../b",
        "a/..",
        "/x",
        "/",
        "a\\b",
        "..\\x",
        "C:/x",
        "C:\\x",
        "\\\\server\\share",
        "//server/share",
        "",
        "a b",
    ];
    for (pointer, make) in fields {
        for v in good {
            let r = manifest::validate(&make(v), &limits);
            assert!(r.valid, "{pointer} = {v:?} is refused: {:?}", r.problems);
        }
        for v in bad {
            let r = manifest::validate(&make(v), &limits);
            assert!(!r.valid, "{pointer} = {v:?} is accepted");
            assert!(
                r.problems.iter().all(|p| p.pointer == pointer),
                "{pointer} = {v:?}: {:?}",
                r.problems
            );
        }
    }
    let glob = |v: &str| json!({ "runtime": "static", "telemetry": { "source_maps": v } });
    assert!(manifest::validate(&glob("**/*.map"), &limits).valid);
    assert!(!manifest::validate(&glob("../**/*.map"), &limits).valid);
    assert!(!manifest::validate(&glob("/maps/*.map"), &limits).valid);
}

/// The secret scan finds a value raw, in hex, percent-encoded and in base64 at any byte
/// position of a longer base64 text, and does not fire on other text.
#[test]
fn the_secret_scan_finds_encoded_values() {
    let secret = "sk_test_ruum/fixture+42".to_string();
    let secrets = vec![secret.clone()];
    let b64 = |data: &[u8]| {
        let out = std::process::Command::new("python3")
            .args(["-I", "-c", "import sys, base64; sys.stdout.write(base64.b64encode(bytes.fromhex(sys.argv[1])).decode())"])
            .arg(data.iter().map(|b| format!("{b:02x}")).collect::<String>())
            .output()
            .expect("python3 is needed by this test (CI has it)");
        String::from_utf8(out.stdout).expect("utf-8")
    };
    assert_eq!(apps::leaked(&format!("x {secret} y"), &secrets), Some(0));
    let hex: String = secret.bytes().map(|b| format!("{b:02x}")).collect();
    assert_eq!(apps::leaked(&hex, &secrets), Some(0));
    assert_eq!(apps::leaked(&hex.to_uppercase(), &secrets), Some(0));
    assert_eq!(
        apps::leaked("sk_test_ruum%2Ffixture%2B42", &secrets),
        Some(0)
    );
    for before in ["", "a", "ab", "abc", "{\"k\":\""] {
        let data = [before.as_bytes(), secret.as_bytes(), b"\"} tail"].concat();
        let enc = b64(&data);
        assert_eq!(
            apps::leaked(&enc, &secrets),
            Some(0),
            "after {before:?}: {enc}"
        );
        let url = enc.replace('+', "-").replace('/', "_");
        assert_eq!(
            apps::leaked(&url, &secrets),
            Some(0),
            "url-safe after {before:?}"
        );
    }
    assert_eq!(apps::leaked("sk_test_ruum fixture 42", &secrets), None);
    assert_eq!(
        apps::leaked(&b64(b"another value entirely"), &secrets),
        None
    );
}

/// Runs a standard tool and returns its standard output; panics when it fails or is missing.
fn tool(cmd: &str, args: &[&str]) -> String {
    let out = std::process::Command::new(cmd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{cmd} is needed by this test (CI has it): {e}"));
    assert!(
        out.status.success(),
        "{cmd} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The hostile archives of the cases, read by tools that did not write them (`gzip -t`, `tar
/// -tvzf`, Python's `zipfile`): each is well formed and holds the entry it is meant to hold, so
/// a refusal in a case is for the hostile entry, not for broken framing. The bomb is decoded to
/// its end by `gzip -t` (4 GiB, CRC and length).
#[test]
fn archives_decode_with_standard_tools() {
    let dir = std::env::temp_dir().join(format!("ruum-t4a-archives-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the folder");
    let m: &[u8] = b"runtime: static\n";
    let i: &[u8] = b"<h1>x</h1>";
    let manifest = || TarEntry::file("app.yaml", m);
    // (file, archive, entries as (type letter of `tar -tv`, the start of the name column))
    type Listed<'a> = Vec<(char, &'a str)>;
    let tars: Vec<(&str, Vec<u8>, Listed)> = vec![
        (
            "site.tar.gz",
            apps_archive::tar_gz(&[("app.yaml", m), ("index.html", i)]),
            vec![('-', "app.yaml"), ('-', "index.html")],
        ),
        (
            "slip.tar.gz",
            apps_archive::tar_gz(&[("app.yaml", m), ("../outside.html", i)]),
            vec![('-', "../outside.html")],
        ),
        (
            "deep.tar.gz",
            apps_archive::tar_gz(&[("app.yaml", m), ("dist/../../outside.html", i)]),
            vec![('-', "dist/../../outside.html")],
        ),
        (
            "symlink.tar.gz",
            apps_archive::gzip(&apps_archive::tar(&[
                manifest(),
                TarEntry::special("index.html", TarKind::SymLink, "/etc/passwd"),
            ])),
            vec![('l', "index.html -> /etc/passwd")],
        ),
        (
            "hardlink.tar.gz",
            apps_archive::gzip(&apps_archive::tar(&[
                manifest(),
                TarEntry::special("index.html", TarKind::HardLink, "../../secret"),
            ])),
            // GNU tar lists the target without its leading ../ (it strips them).
            vec![('h', "index.html link to ")],
        ),
        (
            "devices.tar.gz",
            apps_archive::gzip(&apps_archive::tar(&[
                manifest(),
                TarEntry::special("char", TarKind::CharDevice, ""),
                TarEntry::special("block", TarKind::BlockDevice, ""),
                TarEntry::special("fifo", TarKind::Fifo, ""),
            ])),
            vec![('c', "char"), ('b', "block"), ('p', "fifo")],
        ),
    ];
    for (name, bytes, entries) in &tars {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write");
        let path = path.to_str().expect("utf-8 path");
        tool("gzip", &["-t", path]);
        let listing = tool("tar", &["-tvzf", path]);
        for (kind, entry) in entries {
            assert!(
                listing
                    .lines()
                    .any(|l| l.starts_with(*kind) && l.contains(&format!(" {entry}"))),
                "{name}: tar does not list {kind} {entry:?}:\n{listing}"
            );
        }
    }
    let zips: Vec<(&str, Vec<u8>, &str, u32)> = vec![
        (
            "slip.zip",
            apps_archive::zip(&[
                ZipEntry::file("app.yaml", m),
                ZipEntry::file("../../outside.html", i),
            ]),
            "../../outside.html",
            0,
        ),
        (
            "abs.zip",
            apps_archive::zip(&[
                ZipEntry::file("app.yaml", m),
                ZipEntry::file("/etc/ruum.html", i),
            ]),
            "/etc/ruum.html",
            0,
        ),
        (
            "symlink.zip",
            apps_archive::zip(&[
                ZipEntry::file("app.yaml", m),
                ZipEntry {
                    name: "index.html",
                    data: b"/etc/passwd",
                    unix_mode: Some(0o120_777),
                },
            ]),
            "index.html",
            0o120_777,
        ),
    ];
    let script = "import sys, zipfile\n\
        z = zipfile.ZipFile(sys.argv[1])\n\
        assert z.testzip() is None\n\
        for e in z.infolist():\n    print(oct(e.external_attr >> 16), e.filename)\n";
    for (name, bytes, entry, mode) in &zips {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write");
        let listing = tool(
            "python3",
            &["-I", "-c", script, path.to_str().expect("utf-8 path")],
        );
        // Python's oct() and Rust's {:#o} agree: 0o0, 0o120777.
        let want = format!("{mode:#o} {entry}");
        assert!(
            listing.lines().any(|l| l == want),
            "{name}: zipfile does not list {want:?}:\n{listing}"
        );
    }
    // The escaping names, as Python reads them (GNU tar rewrites `/` and `\` names when it
    // lists them; Python's readers keep the stored name).
    for (name, evil) in apps_archive::ZIP_ESCAPES {
        let path = dir.join(name);
        std::fs::write(
            &path,
            apps_archive::zip(&[ZipEntry::file("app.yaml", m), ZipEntry::file(evil, i)]),
        )
        .expect("write");
        let listing = tool(
            "python3",
            &["-I", "-c", script, path.to_str().expect("utf-8 path")],
        );
        assert!(
            listing.lines().any(|l| l == format!("0o0 {evil}")),
            "{name}: zipfile does not list {evil:?}:\n{listing}"
        );
    }
    let tar_script = "import sys, tarfile\n\
        t = tarfile.open(sys.argv[1], 'r:gz')\n\
        names = [m.name for m in t.getmembers() if m.isfile()]\n\
        print(len(names))\n\
        for n in names[:5]:\n    print(n)\n";
    for (name, evil) in apps_archive::TAR_ESCAPES {
        let path = dir.join(name);
        std::fs::write(&path, apps_archive::tar_gz(&[("app.yaml", m), (evil, i)])).expect("write");
        let path = path.to_str().expect("utf-8 path");
        tool("gzip", &["-t", path]);
        let listing = tool("python3", &["-I", "-c", tar_script, path]);
        assert!(
            listing.lines().any(|l| l == *evil),
            "{name}: tarfile does not list {evil:?}:\n{listing}"
        );
    }
    // The zip bomb: zipfile expands it to its end (2 GiB, CRC checked) and reads the sizes.
    let zbomb = dir.join("bomb.zip");
    std::fs::write(&zbomb, apps_archive::bomb_zip()).expect("write");
    let zip_bomb_script = "import sys, zipfile\n\
        z = zipfile.ZipFile(sys.argv[1])\n\
        assert z.testzip() is None\n\
        e = z.getinfo('zeros.bin')\n\
        print(e.compress_type, e.file_size, e.compress_size)\n";
    let listing = tool(
        "python3",
        &[
            "-I",
            "-c",
            zip_bomb_script,
            zbomb.to_str().expect("utf-8 path"),
        ],
    );
    let fields: Vec<u64> = listing
        .split_whitespace()
        .map(|x| x.parse().expect("a number"))
        .collect();
    assert_eq!(fields[..2], [8, u64::from(apps_archive::ZIP_BOMB_BYTES)]);
    assert!(fields[2] < 3 << 20, "compressed {}", fields[2]);
    // The entry-count bombs: every file is there.
    let files = apps_archive::COUNT_BOMB_FILES + 1;
    let many_zip = dir.join("many.zip");
    std::fs::write(&many_zip, apps_archive::count_bomb_zip()).expect("write");
    let listing = tool(
        "python3",
        &[
            "-I",
            "-c",
            "import sys, zipfile\nz = zipfile.ZipFile(sys.argv[1])\nassert z.testzip() is None\nprint(len(z.infolist()))\n",
            many_zip.to_str().expect("utf-8 path"),
        ],
    );
    assert_eq!(listing.trim(), files.to_string());
    let many_tar = dir.join("many.tar.gz");
    std::fs::write(&many_tar, apps_archive::count_bomb_tar_gz()).expect("write");
    let many_tar = many_tar.to_str().expect("utf-8 path");
    tool("gzip", &["-t", many_tar]);
    let listing = tool("python3", &["-I", "-c", tar_script, many_tar]);
    assert_eq!(listing.lines().next(), Some(files.to_string().as_str()));
    let bomb = dir.join("bomb.tar.gz");
    std::fs::write(&bomb, apps_archive::bomb_tar_gz()).expect("write");
    tool("gzip", &["-t", bomb.to_str().expect("utf-8 path")]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Writes every archive of the cases to `$RUUM_ARCHIVE_DIR`, so a person can check them with
/// `gzip -t`, `tar -tvzf` and `unzip -l` (`cargo test -p ruum-testkit --test apps_meta --
/// --ignored dump_archives`).
#[test]
#[ignore]
fn dump_archives() {
    let dir =
        std::path::PathBuf::from(std::env::var("RUUM_ARCHIVE_DIR").expect("RUUM_ARCHIVE_DIR"));
    std::fs::create_dir_all(&dir).expect("the folder");
    let files: [(&str, &[u8]); 2] = [
        ("app.yaml", b"runtime: static\n"),
        ("index.html", b"<h1>x</h1>"),
    ];
    let mut out = vec![
        ("site.tar.gz", apps_archive::tar_gz(&files)),
        (
            "site.zip",
            apps_archive::zip(&[ZipEntry::file("app.yaml", b"runtime: static\n")]),
        ),
        (
            "slip.tar.gz",
            apps_archive::tar_gz(&[("../outside.html", b"x")]),
        ),
        (
            "symlink.zip",
            apps_archive::zip(&[ZipEntry {
                name: "index.html",
                data: b"/etc/passwd",
                unix_mode: Some(0o120_777),
            }]),
        ),
        (
            "devices.tar.gz",
            apps_archive::gzip(&apps_archive::tar(&[
                TarEntry::special("char", TarKind::CharDevice, ""),
                TarEntry::special("block", TarKind::BlockDevice, ""),
                TarEntry::special("fifo", TarKind::Fifo, ""),
                TarEntry::special("hard", TarKind::HardLink, "../../secret"),
            ])),
        ),
        ("bomb.tar.gz", apps_archive::bomb_tar_gz()),
        ("bomb.zip", apps_archive::bomb_zip()),
        ("many.zip", apps_archive::count_bomb_zip()),
        ("many.tar.gz", apps_archive::count_bomb_tar_gz()),
    ];
    for &(name, evil) in apps_archive::ZIP_ESCAPES {
        out.push((
            name,
            apps_archive::zip(&[
                ZipEntry::file("app.yaml", b"runtime: static\n"),
                ZipEntry::file(evil, b"x"),
            ]),
        ));
    }
    for &(name, evil) in apps_archive::TAR_ESCAPES {
        out.push((
            name,
            apps_archive::tar_gz(&[("app.yaml", b"runtime: static\n"), (evil, b"x")]),
        ));
    }
    for (name, bytes) in out {
        std::fs::write(dir.join(name), bytes).expect("write");
    }
}
