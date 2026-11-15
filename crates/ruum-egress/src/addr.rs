//! The address rules (decision 0006 G point 1), the deny list `EGRESS_DENY`, CIDR prefixes, and
//! the parsing of the host of a `CONNECT` request (names, IP literals and the numeric forms a
//! resolver accepts).

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The address the kernel connects to and every rule checks: an IPv4-mapped IPv6 address
/// (`::ffff:a.b.c.d`) is its IPv4 address. This is the only place that maps, so the public
/// rules, the deny list and the connect all see the same form.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Whether a canonical address may be a destination. Callers pass [`canonical`]'s result.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    let refused = o[0] == 0 // 0.0.0.0/8 ("this network", the unspecified address)
        || o[0] == 10 // 10.0.0.0/8 private
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64.0.0/10 CGNAT
        || o[0] == 127 // 127.0.0.0/8 loopback
        || (o[0] == 169 && o[1] == 254) // 169.254.0.0/16 link-local, metadata 169.254.169.254
        || (o[0] == 172 && (o[1] & 0xf0) == 16) // 172.16.0.0/12 private
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 IETF protocol assignments
        || (o[0] == 192 && o[1] == 0 && o[2] == 2) // 192.0.2.0/24 documentation
        || (o[0] == 192 && o[1] == 88 && o[2] == 99) // 192.88.99.0/24 6to4 relay anycast
        || (o[0] == 192 && o[1] == 168) // 192.168.0.0/16 private
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18.0.0/15 benchmarking
        || (o[0] == 198 && o[1] == 51 && o[2] == 100) // 198.51.100.0/24 documentation
        || (o[0] == 203 && o[1] == 0 && o[2] == 113) // 203.0.113.0/24 documentation
        || (o[0] & 0xf0) == 224 // 224.0.0.0/4 multicast
        || o[0] >= 240; // 240.0.0.0/4 reserved, 255.255.255.255 broadcast
    !refused
}

fn is_public_v6(v6: Ipv6Addr) -> bool {
    let s = v6.segments();
    // Only global unicast 2000::/3 can pass. That alone refuses loopback `::1`, the unspecified
    // `::`, IPv4-compatible `::a.b.c.d` (whose IPv4 form is checked below too), NAT64
    // `64:ff9b::/96` and `64:ff9b:1::/48` (they translate to any IPv4), discard `100::/64`,
    // unique local `fc00::/7` (with the metadata address `fd00:ec2::254`), link-local
    // `fe80::/10` and multicast `ff00::/8`.
    if (s[0] & 0xe000) != 0x2000 {
        return false;
    }
    let refused = (s[0] == 0x2001 && s[1] < 0x0200) // 2001::/23: Teredo 2001::/32, benchmarking 2001:2::/48, ORCHID
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
        || s[0] == 0x2002 // 2002::/16 6to4 (embeds any IPv4)
        || (s[0] == 0x3fff && (s[1] & 0xf000) == 0); // 3fff::/20 documentation: 3fff:0000:: to 3fff:0fff:ffff:...
    !refused
}

/// The IPv4 form of an IPv4-compatible IPv6 address (`::a.b.c.d`, deprecated), except `::` and
/// `::1`. Such an answer is outside `2000::/3` and refused anyway; the deny list sees this form
/// too, so a deny entry of an IPv4 address also names its compatible form.
fn ipv4_compatible(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            let v4 = Ipv4Addr::from(u32::try_from(u128::from(v6) & 0xffff_ffff).ok()?);
            (s[..6].iter().all(|x| *x == 0) && u32::from(v4) > 1).then_some(v4)
        }
        IpAddr::V4(_) => None,
    }
}

/// An address or a CIDR prefix (`a.b.c.d`, `a.b.c.d/n`, `x::y`, `x::/n`). An IPv4-mapped prefix
/// (`::ffff:a.b.c.d/n`, n >= 96) is stored as its IPv4 prefix. Host bits are cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    len: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Result<Cidr, String> {
        let (a, l) = match s.split_once('/') {
            Some((a, l)) => (a, Some(l)),
            None => (s, None),
        };
        let ip: IpAddr = a
            .parse()
            .map_err(|_| format!("`{s}` is not an IP address or CIDR prefix"))?;
        let max: u8 = if ip.is_ipv4() { 32 } else { 128 };
        let mut len = match l {
            None => max,
            Some(l) => {
                if l.is_empty() || !l.bytes().all(|b| b.is_ascii_digit()) || l.len() > 3 {
                    return Err(format!("`{s}` has no valid prefix length"));
                }
                let n: u8 = l
                    .parse()
                    .map_err(|_| format!("`{s}` has no valid prefix length"))?;
                if n > max {
                    return Err(format!("`{s}`: the prefix length is above {max}"));
                }
                n
            }
        };
        let mut net = ip;
        if let IpAddr::V6(v6) = ip
            && let Some(v4) = v6.to_ipv4_mapped()
        {
            if len < 96 {
                return Err(format!(
                    "`{s}`: an IPv4-mapped prefix needs a length of 96 or more"
                ));
            }
            net = IpAddr::V4(v4);
            len -= 96;
        }
        Ok(Cidr {
            net: mask(net, len),
            len,
        })
    }

    pub fn prefix_len(&self) -> u8 {
        self.len
    }

    pub fn is_ipv4(&self) -> bool {
        self.net.is_ipv4()
    }

    /// Whether `ip` is inside the prefix. No mapping: callers pass [`canonical`] forms.
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (ip, self.net) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                mask(ip, self.len) == self.net
            }
            _ => false,
        }
    }

    /// Whether the two prefixes share an address.
    pub fn overlaps(&self, other: &Cidr) -> bool {
        let shorter = self.len.min(other.len);
        self.net.is_ipv4() == other.net.is_ipv4()
            && mask(self.net, shorter) == mask(other.net, shorter)
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.net, self.len)
    }
}

fn mask(ip: IpAddr, len: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let m = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
            IpAddr::V4(Ipv4Addr::from(u32::from(v4) & m))
        }
        IpAddr::V6(v6) => {
            let m = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
            IpAddr::V6(Ipv6Addr::from(u128::from(v6) & m))
        }
    }
}

/// `EGRESS_DENY`: the platform ranges and the host's public addresses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DenyList(Vec<Cidr>);

impl DenyList {
    /// Parses addresses and CIDR prefixes separated by commas or white space. An entry that does
    /// not parse is an error (backlog S-38: never dropped silently); so is an empty entry
    /// between two commas.
    pub fn parse(s: &str) -> Result<DenyList, String> {
        let mut out = Vec::new();
        if s.trim().is_empty() {
            return Ok(DenyList(out));
        }
        for part in s.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(
                    "EGRESS_DENY has an empty entry (two commas, or a comma at an end)".to_string(),
                );
            }
            for tok in part.split_whitespace() {
                match Cidr::parse(tok) {
                    Ok(c) => out.push(c),
                    Err(e) => return Err(format!("EGRESS_DENY entry: {e}")),
                }
            }
        }
        Ok(DenyList(out))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn entries(&self) -> &[Cidr] {
        &self.0
    }

    /// Whether a canonical address is denied (also through its IPv4-compatible form).
    pub fn contains(&self, ip: IpAddr) -> bool {
        let compat = ipv4_compatible(ip).map(IpAddr::V4);
        self.0
            .iter()
            .any(|d| d.contains(ip) || compat.is_some_and(|c| d.contains(c)))
    }
}

/// The host of a `CONNECT` request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    /// A DNS name: lowercase, without a trailing dot.
    Name(String),
    /// An IP literal, or a name made of numbers that a resolver reads as an IPv4 address.
    Ip(IpAddr),
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Host::Name(n) => f.write_str(n),
            Host::Ip(IpAddr::V6(v6)) => write!(f, "[{v6}]"),
            Host::Ip(ip) => write!(f, "{ip}"),
        }
    }
}

/// Why a host was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadHost {
    /// Not a name, not an IP literal: the request is malformed (`400`).
    Malformed,
    /// Made of numbers, but not an IPv4 address a resolver would accept (`1.2.3.256`). Refused
    /// as an address (`403 address`): it never reaches a resolver.
    BadNumber,
}

/// Parses the host part of a `CONNECT` target. Bracketed or bare IPv6 literals, IPv4 literals
/// and every numeric form of `inet_aton` (decimal `2852039166`, octal `0251.0376.0251.0376`,
/// hex `0xa9fea9fe`, short forms `127.1`) become [`Host::Ip`]; a name is lowercased and loses one
/// trailing dot.
pub fn parse_host(raw: &str) -> Result<Host, BadHost> {
    if let Some(inner) = raw.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or(BadHost::Malformed)?;
        return inner
            .parse::<Ipv6Addr>()
            .map(|v6| Host::Ip(IpAddr::V6(v6)))
            .map_err(|_| BadHost::Malformed);
    }
    if raw.contains(':') {
        return raw
            .parse::<Ipv6Addr>()
            .map(|v6| Host::Ip(IpAddr::V6(v6)))
            .map_err(|_| BadHost::Malformed);
    }
    let name = raw.strip_suffix('.').unwrap_or(raw).to_ascii_lowercase();
    if name.is_empty() || name.len() > 253 {
        return Err(BadHost::Malformed);
    }
    let labels: Vec<&str> = name.split('.').collect();
    if labels.iter().all(|l| is_numeric_label(l)) {
        return inet_aton(&labels)
            .map(|v4| Host::Ip(IpAddr::V4(v4)))
            .ok_or(BadHost::BadNumber);
    }
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    };
    if labels.iter().all(label_ok) {
        Ok(Host::Name(name))
    } else {
        Err(BadHost::Malformed)
    }
}

/// A label `inet_aton` reads as a number: decimal digits, or `0x` with hex digits.
fn is_numeric_label(l: &str) -> bool {
    if let Some(h) = l.strip_prefix("0x") {
        h.bytes().all(|b| b.is_ascii_hexdigit())
    } else {
        !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit())
    }
}

/// `inet_aton` of glibc: one to four parts, each decimal, octal (a leading `0`) or hex (`0x`);
/// the last part fills the remaining bytes.
fn inet_aton(parts: &[&str]) -> Option<Ipv4Addr> {
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut nums = Vec::with_capacity(parts.len());
    for p in parts {
        let v = if let Some(h) = p.strip_prefix("0x") {
            if h.is_empty() {
                0
            } else if h.len() > 8 {
                return None;
            } else {
                u64::from_str_radix(h, 16).ok()?
            }
        } else if p.len() > 1 && p.starts_with('0') {
            if p.len() > 12 {
                return None;
            }
            u64::from_str_radix(&p[1..], 8).ok()?
        } else {
            if p.len() > 10 {
                return None;
            }
            p.parse::<u64>().ok()?
        };
        nums.push(v);
    }
    let (last, head) = nums.split_last()?;
    if head.iter().any(|v| *v > 255) {
        return None;
    }
    let rest_bits = 8 * (4 - head.len() as u32);
    if rest_bits < 64 && *last >= (1u64 << rest_bits) {
        return None;
    }
    let mut out: u32 = 0;
    for (i, v) in head.iter().enumerate() {
        out |= (*v as u32) << (24 - 8 * i as u32);
    }
    out |= *last as u32;
    Some(Ipv4Addr::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(s: &str) -> bool {
        is_public(canonical(s.parse().unwrap()))
    }

    #[test]
    fn ipv4_rules() {
        for s in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "127.255.0.1",
            "169.254.169.254",
            "169.254.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!public(s), "{s} must be refused");
        }
        for s in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "198.17.255.255",
            "198.20.0.1",
            "223.255.255.255",
            "104.16.1.1",
        ] {
            assert!(public(s), "{s} must pass");
        }
    }

    #[test]
    fn ipv6_rules() {
        for s in [
            "::",
            "::1",
            "::127.0.0.1",
            "::8.8.8.8",
            "64:ff9b::a9fe:a9fe",
            "100::1",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "febf::1",
            "ff02::1",
            "2001::1",
            "2001:0:ffff::1",
            "2001:2::1",
            "2001:1ff::1",
            "2001:db8::1",
            "2002::1",
            "2002:a9fe:a9fe::1",
            "3fff::1",
            "3fff:fff:ffff::1",
            "4000::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
        ] {
            assert!(!public(s), "{s} must be refused");
        }
        // The exact borders: 3fff::/20 ends at 3fff:0fff:..., 2001::/23 at 2001:1ff:...
        for s in [
            "2001:200::1",
            "2001:4860:4860::8888",
            "2606:4700::1111",
            "3fff:1000::1",
            "3ff0::1",
            "3ffe::1",
            "2000::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(public(s), "{s} must pass");
        }
    }

    #[test]
    fn mapped_is_checked_as_ipv4() {
        let ip: IpAddr = "::ffff:95.217.0.10".parse().unwrap();
        assert_eq!(canonical(ip), "95.217.0.10".parse::<IpAddr>().unwrap());
        let deny = DenyList::parse("95.217.0.10").unwrap();
        assert!(deny.contains(canonical(ip)));
        let compat: IpAddr = "::95.217.0.10".parse().unwrap();
        assert!(deny.contains(canonical(compat)));
    }

    #[test]
    fn deny_list_parses_and_refuses_bad_entries() {
        let d = DenyList::parse("95.217.0.10, 2a01:4f9:c010:1234::/64 37.27.0.0/24").unwrap();
        assert_eq!(d.entries().len(), 3);
        assert!(d.contains("2a01:4f9:c010:1234:ffff::5".parse().unwrap()));
        assert!(!d.contains("2a01:4f9:c010:1235::1".parse().unwrap()));
        assert!(d.contains("37.27.0.200".parse().unwrap()));
        assert!(!d.contains("37.27.1.1".parse().unwrap()));
        for bad in [
            "95.217.0.300",
            "95.217.0.10/33",
            "2a01::/129",
            "nope",
            "1.2.3.4,,5.6.7.8",
            "1.2.3.4,",
            "1.2.3.4/",
            "1.2.3.4/+8",
            "::ffff:1.2.3.4/64",
        ] {
            assert!(DenyList::parse(bad).is_err(), "{bad} must not parse");
        }
        assert!(DenyList::parse("").unwrap().is_empty());
        assert!(DenyList::parse("  ").unwrap().is_empty());
        let mapped = DenyList::parse("::ffff:95.217.0.0/120").unwrap();
        assert!(mapped.contains("95.217.0.99".parse().unwrap()));
    }

    #[test]
    fn cidr_overlap_and_zero_length() {
        let a = Cidr::parse("10.0.0.0/8").unwrap();
        let b = Cidr::parse("10.1.0.0/16").unwrap();
        let c = Cidr::parse("11.0.0.0/8").unwrap();
        assert!(a.overlaps(&b) && b.overlaps(&a));
        assert!(!a.overlaps(&c));
        let all = Cidr::parse("0.0.0.0/0").unwrap();
        assert!(all.contains("8.8.8.8".parse().unwrap()));
        assert!(!all.contains("::1".parse().unwrap()));
        assert_eq!(Cidr::parse("10.1.2.3/8").unwrap().to_string(), "10.0.0.0/8");
    }

    #[test]
    fn numeric_hosts_parse_like_a_resolver() {
        let ip = |s: &str| match parse_host(s) {
            Ok(Host::Ip(ip)) => ip.to_string(),
            other => panic!("{s}: {other:?}"),
        };
        assert_eq!(ip("2852039166"), "169.254.169.254");
        assert_eq!(ip("0251.0376.0251.0376"), "169.254.169.254");
        assert_eq!(ip("0xa9fea9fe"), "169.254.169.254");
        assert_eq!(ip("0xA9FEA9FE"), "169.254.169.254");
        assert_eq!(ip("0xa9.0xfe.0xa9.0xfe"), "169.254.169.254");
        assert_eq!(ip("169.254.43518"), "169.254.169.254");
        assert_eq!(ip("127.1"), "127.0.0.1");
        assert_eq!(ip("0x7f.1"), "127.0.0.1");
        assert_eq!(ip("017700000001"), "127.0.0.1");
        assert_eq!(ip("127.0.0.1."), "127.0.0.1");
        assert_eq!(ip("0"), "0.0.0.0");
        assert_eq!(ip("[::1]"), "::1");
        assert_eq!(ip("::1"), "::1");
        assert_eq!(ip("[::ffff:127.0.0.1]"), "::ffff:127.0.0.1");
        for bad in ["1.2.3.256", "4294967296", "1.2.3.4.5", "08.1.1.1", "256.1"] {
            assert_eq!(parse_host(bad), Err(BadHost::BadNumber), "{bad}");
        }
        for bad in [
            "",
            ".",
            "a..b",
            "a b",
            "user@host",
            "[::1",
            "[nope]",
            "fe80::1%eth0",
        ] {
            assert_eq!(parse_host(bad), Err(BadHost::Malformed), "{bad}");
        }
        assert_eq!(
            parse_host("Registry.NPMjs.org."),
            Ok(Host::Name("registry.npmjs.org".into()))
        );
        assert_eq!(parse_host("1e100.net"), Ok(Host::Name("1e100.net".into())));
    }
}
