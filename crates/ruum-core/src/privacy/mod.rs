//! The privacy border (design 8.5, `docs/STEP2.md` "The engine security model").
//!
//! This module holds three things that both database engines, the query layer and (in step 3)
//! telemetry share:
//!
//! - The typed markers of `contracts/openapi.yaml`: [`HiddenValue`], [`Pseudonym`] and
//!   [`LiteralValue`]. They serialize exactly as the contract schemas.
//! - The pseudonym function: [`Pseudonymizer`] and its one implementation
//!   [`HmacPseudonymizer`], keyed by the [`PseudonymKey`] of a database (the tenant key, or the
//!   key derived for `databases.key_node_id`, decision 0004-25):
//!
//!   ```text
//!   pseudonym(v) = 'p_' || first 12 hex characters of HMAC-SHA-256(key, canonical_text(v))
//!   ```
//!
//!   The Postgres engine computes the same function in SQL (`ruum_mask.pseudonym(<type>)`,
//!   card Q-2); SQLite calls this Rust code (card E-3 registers it, card Q-3 uses it). Both
//!   must give the same pseudonym for the same value: the unit tests hold the SQL output of
//!   spike S-1 as test vectors.
//! - The [`MaskingRules`] hook: the classes of a database changed, so rebuild what depends on
//!   them (the lens views). Card K-1 calls it; Q-2 (`postgres.rs`) and Q-3 (`sqlite.rs`)
//!   implement it.
//!
//! # canonical_text (decision 0004-34 e)
//!
//! The text form of a value, with a fixed format that no session setting changes:
//!
//! | Type | Canonical text |
//! |---|---|
//! | `text`, `varchar`, `char(n)` (as the cast to `text` gives it: trailing blanks removed) | as is |
//! | `int2`, `int4`, `int8` (SQLite `INTEGER`) | decimal |
//! | `numeric` | without trailing fraction zeros (`trim_scale`); a zero has no sign; `NaN`, `Infinity`, `-Infinity` |
//! | `float8` (SQLite `REAL`), `float4` | shortest form that reads back exactly as that type, in the layout of `float8out` / `float4out` with `extra_float_digits = 1`: fixed notation for a decimal exponent in [-4, 15) (float4: [-4, 6)), else `d.ddde+XX`; `NaN`, `Infinity`, `-Infinity`, `-0` |
//! | `timestamptz`, `timestamp` | `YYYY-MM-DDTHH:MM:SS[.ffffff without trailing zeros]Z` in UTC (a `timestamp` is taken as UTC) |
//! | `date` | `YYYY-MM-DD` |
//! | `bytea` (SQLite `BLOB`) | lowercase hex without `\x` |
//! | `boolean` | `true` / `false` |
//! | `uuid` | lowercase, with hyphens |
//!
//! An infinite `timestamptz`, `timestamp` or `date` has the canonical text `infinity` or
//! `-infinity` (the SQL side uses `v::text` for them): pass it as [`SqlValue::Other`].
//!
//! The SQL side needs one typed overload per row of the table. Spike S-1 built `text`,
//! `bigint`, `numeric`, `double precision`, `timestamptz`, `date`, `bytea`, `boolean` and
//! `uuid`. An `int2` or `int4` value does not reach the `bigint` overload: PostgreSQL resolves
//! it to `double precision`, the preferred type of the numeric category. The text is the same
//! today (every `int4` value is exact in `float8` and prints in decimal), but card Q-2 must not
//! depend on it: it adds `int2` and `int4` overloads, or casts to `bigint` in the lens view.
//! Card Q-2 also adds `real` (the implicit cast to `double precision` adds digits: `0.1::float4`
//! is `0.10000000149011612` as a `float8`) and `timestamp` (a cast to `timestamptz` follows the
//! session's `TimeZone`).
//!
//! # Types of the generic overload
//!
//! A `personal` column of any other type uses the generic overload: the output function of the
//! type (`v::text`) with every output setting fixed in the function (`DateStyle = 'ISO, YMD'`,
//! `IntervalStyle = postgres`, `TimeZone = UTC`, `extra_float_digits = 1`, `bytea_output =
//! hex`, `lc_monetary = C`). On the Rust side that text is [`SqlValue::Other`], hashed as is.
//! These types are:
//!
//! - times without a date and intervals: `time`, `timetz`, `interval`;
//! - `money` (its output follows `lc_monetary`, so the function fixes it);
//! - documents: `json`, `jsonb`, `xml`;
//! - network types: `inet`, `cidr`, `macaddr`, `macaddr8`;
//! - bit strings: `bit`, `varbit`;
//! - arrays of any type, enum types, composite (row) types, domains over a type without a
//!   typed overload, range and multirange types;
//! - geometric types: `point`, `line`, `lseg`, `box`, `path`, `polygon`, `circle`;
//! - text search types: `tsvector`, `tsquery`;
//! - system types: `pg_lsn`, `oid` and the `reg*` types, `xid8`;
//! - types of the manifest's extensions (for example `citext`, `hstore`, `ltree`, `cube`,
//!   `geometry`, `geography`, `vector`).
//!
//! A domain over a type with a typed overload uses that overload. SQLite has only the
//! storage classes `INTEGER`, `REAL`, `TEXT` and `BLOB`, which map to [`SqlValue::Int`],
//! [`SqlValue::Float8`], [`SqlValue::Text`] and [`SqlValue::Bytes`].

pub mod keys;
pub mod postgres;
pub mod sqlite;

use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::de::Error as _;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use time::{Date, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};
use uuid::Uuid;

use crate::{NodeId, Result, TenantId};

/// The reserved key of a marker object (`$ruum`).
pub const MARKER_KEY: &str = "$ruum";

/// The number of hex characters of a pseudonym after `p_` (48 bits).
pub const PSEUDONYM_HEX_LEN: usize = 12;

// ------------------------------------------------------------------------------------ markers

/// The class of a hidden value (`HiddenValue.class`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HiddenClass {
    /// A secret: agents, and people without `manage`, never see it.
    Secret,
    /// A personal value where pseudonyms are not allowed, or a value that was removed.
    Personal,
}

/// The hidden-value marker: `{"$ruum": "hidden", "class": "secret"}`. A value that the caller
/// may not see. Never a look-alike string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HiddenValue {
    pub class: HiddenClass,
}

impl HiddenValue {
    pub const SECRET: HiddenValue = HiddenValue {
        class: HiddenClass::Secret,
    };
    pub const PERSONAL: HiddenValue = HiddenValue {
        class: HiddenClass::Personal,
    };
}

/// A pseudonym marker: `{"$ruum": "pseudonym", "id": "p_8f3a91c2d4e5"}`. The `id` matches
/// `^p_[0-9a-f]{6,32}$`; this platform makes `p_` plus [`PSEUDONYM_HEX_LEN`] characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pseudonym {
    id: String,
}

impl Pseudonym {
    /// Checks the contract pattern `^p_[0-9a-f]{6,32}$`.
    pub fn parse(id: &str) -> Option<Pseudonym> {
        let hex = id.strip_prefix("p_")?;
        let ok = (6..=32).contains(&hex.len())
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        ok.then(|| Pseudonym { id: id.to_string() })
    }

    /// The pseudonym, for example `p_8f3a91c2d4e5`.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl fmt::Display for Pseudonym {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id)
    }
}

/// A real JSON object that has the key `$ruum`, wrapped so that it is never read as a marker:
/// `{"$ruum": "literal", "value": {...}}`.
#[derive(Clone, Debug, PartialEq)]
pub struct LiteralValue {
    pub value: serde_json::Value,
}

impl LiteralValue {
    /// Turns a plain JSON value into a result cell (`QueryCell`): an object with the key
    /// `$ruum` is wrapped in a [`LiteralValue`]; every other value stays as it is.
    pub fn cell(value: serde_json::Value) -> serde_json::Value {
        let needs_wrap =
            matches!(&value, serde_json::Value::Object(map) if map.contains_key(MARKER_KEY));
        if needs_wrap {
            serde_json::to_value(LiteralValue { value }).expect("a JSON value serializes")
        } else {
            value
        }
    }
}

/// One of the three typed markers.
#[derive(Clone, Debug, PartialEq)]
pub enum Marker {
    Hidden(HiddenValue),
    Pseudonym(Pseudonym),
    Literal(LiteralValue),
}

impl From<HiddenValue> for Marker {
    fn from(v: HiddenValue) -> Self {
        Marker::Hidden(v)
    }
}

impl From<Pseudonym> for Marker {
    fn from(v: Pseudonym) -> Self {
        Marker::Pseudonym(v)
    }
}

impl From<LiteralValue> for Marker {
    fn from(v: LiteralValue) -> Self {
        Marker::Literal(v)
    }
}

impl Serialize for HiddenValue {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("HiddenValue", 2)?;
        st.serialize_field(MARKER_KEY, "hidden")?;
        st.serialize_field("class", &self.class)?;
        st.end()
    }
}

impl Serialize for Pseudonym {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Pseudonym", 2)?;
        st.serialize_field(MARKER_KEY, "pseudonym")?;
        st.serialize_field("id", &self.id)?;
        st.end()
    }
}

impl Serialize for LiteralValue {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("LiteralValue", 2)?;
        st.serialize_field(MARKER_KEY, "literal")?;
        st.serialize_field("value", &self.value)?;
        st.end()
    }
}

impl Serialize for Marker {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Marker::Hidden(v) => v.serialize(s),
            Marker::Pseudonym(v) => v.serialize(s),
            Marker::Literal(v) => v.serialize(s),
        }
    }
}

/// The wire form of the markers: tagged by `$ruum`, no other keys (`additionalProperties:
/// false` in the contract).
#[derive(Deserialize)]
#[serde(tag = "$ruum", rename_all = "lowercase", deny_unknown_fields)]
enum WireMarker {
    Hidden { class: HiddenClass },
    Pseudonym { id: String },
    Literal { value: serde_json::Value },
}

impl<'de> Deserialize<'de> for Marker {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(match WireMarker::deserialize(d)? {
            WireMarker::Hidden { class } => Marker::Hidden(HiddenValue { class }),
            WireMarker::Pseudonym { id } => {
                Marker::Pseudonym(Pseudonym::parse(&id).ok_or_else(|| {
                    D::Error::custom("pseudonym id does not match ^p_[0-9a-f]{6,32}$")
                })?)
            }
            WireMarker::Literal { value } => Marker::Literal(LiteralValue { value }),
        })
    }
}

impl<'de> Deserialize<'de> for HiddenValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        match Marker::deserialize(d)? {
            Marker::Hidden(v) => Ok(v),
            _ => Err(D::Error::custom("expected the marker `hidden`")),
        }
    }
}

impl<'de> Deserialize<'de> for Pseudonym {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        match Marker::deserialize(d)? {
            Marker::Pseudonym(v) => Ok(v),
            _ => Err(D::Error::custom("expected the marker `pseudonym`")),
        }
    }
}

impl<'de> Deserialize<'de> for LiteralValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        match Marker::deserialize(d)? {
            Marker::Literal(v) => Ok(v),
            _ => Err(D::Error::custom("expected the marker `literal`")),
        }
    }
}

// -------------------------------------------------------------------------------------- keys

/// The pseudonym key of a database: 32 bytes. The tenant key of `tenant_keys` (card Q-4), or
/// the key derived from it for `databases.key_node_id` ([`PseudonymKey::for_database`]).
///
/// `Debug` never prints the key, and the bytes are overwritten when the key is dropped (best
/// effort: a moved or cloned copy may stay in memory).
#[derive(Clone, PartialEq, Eq)]
pub struct PseudonymKey([u8; 32]);

impl PseudonymKey {
    pub const LEN: usize = 32;

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        PseudonymKey(bytes)
    }

    /// `None` when the slice is not 32 bytes long.
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        <[u8; 32]>::try_from(bytes).ok().map(PseudonymKey)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The key derived from this (tenant) key for a database whose `key_node_id` is set:
    /// `HMAC-SHA-256(tenant key, 'db:' || key_node_id)`, with the node ID in its text form
    /// (lowercase, with hyphens), as the SQL `'db:' || key_node_id` gives it (decision 0004-25).
    pub fn derive_for_database(&self, key_node_id: NodeId) -> PseudonymKey {
        let msg = format!("db:{}", key_node_id.as_uuid().hyphenated());
        PseudonymKey(hmac_sha256(&self.0, msg.as_bytes()))
    }

    /// The pseudonym key of a database from the tenant key and `databases.key_node_id`:
    /// `None` gives the tenant key, a node ID gives the derived key.
    pub fn for_database(tenant_key: &PseudonymKey, key_node_id: Option<NodeId>) -> PseudonymKey {
        match key_node_id {
            None => tenant_key.clone(),
            Some(node) => tenant_key.derive_for_database(node),
        }
    }
}

impl fmt::Debug for PseudonymKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PseudonymKey(<hidden>)")
    }
}

impl Drop for PseudonymKey {
    fn drop(&mut self) {
        // Best effort without `unsafe` (forbidden in the workspace): `black_box` keeps the
        // compiler from removing the write.
        self.0.fill(0);
        std::hint::black_box(&self.0);
    }
}

/// HMAC-SHA-256 (RFC 2104) on `sha2`, for a key of at most one block (64 bytes). Written by
/// hand so the crate needs no new dependency; the tests check it against RFC 4231.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        block_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= block_key[i];
        opad[i] ^= block_key[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

// ---------------------------------------------------------------------------- canonical text

/// A `numeric` value in its canonical form (`trim_scale(v)::text`). Make it with
/// [`Numeric::parse`] from the text that Postgres `numeric_out` gives.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Numeric(String);

impl Numeric {
    /// Parses `[-+]digits[.digits]`, `NaN`, `Infinity` or `-Infinity`. Trailing zeros of the
    /// fraction, a trailing `.`, leading zeros of the integer part and the sign of a zero go.
    /// An exponent is refused (`numeric_out` never writes one).
    pub fn parse(s: &str) -> Option<Numeric> {
        match s {
            "NaN" | "Infinity" | "-Infinity" => return Some(Numeric(s.to_string())),
            "+Infinity" => return Some(Numeric("Infinity".into())),
            _ => {}
        }
        let (neg, rest) = match s.as_bytes().first() {
            Some(b'-') => (true, &s[1..]),
            Some(b'+') => (false, &s[1..]),
            _ => (false, s),
        };
        let (int, frac) = rest.split_once('.').unwrap_or((rest, ""));
        let digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
        if (int.is_empty() && frac.is_empty()) || !digits(int) || !digits(frac) {
            return None;
        }
        let int = int.trim_start_matches('0');
        let frac = frac.trim_end_matches('0');
        let int = if int.is_empty() { "0" } else { int };
        if int == "0" && frac.is_empty() {
            return Some(Numeric("0".into()));
        }
        let mut out = String::with_capacity(int.len() + frac.len() + 2);
        if neg {
            out.push('-');
        }
        out.push_str(int);
        if !frac.is_empty() {
            out.push('.');
            out.push_str(frac);
        }
        Some(Numeric(out))
    }

    /// The canonical text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A value to pseudonymize, by its SQL type (see the table of the module docs).
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue<'a> {
    /// `text`, `varchar`, `char(n)` after the cast to `text`; SQLite `TEXT`.
    Text(&'a str),
    /// `int2`, `int4`, `int8`; SQLite `INTEGER`.
    Int(i64),
    Numeric(Numeric),
    Float4(f32),
    /// `float8`; SQLite `REAL`.
    Float8(f64),
    Timestamptz(OffsetDateTime),
    /// `timestamp` (without time zone), taken as UTC.
    Timestamp(PrimitiveDateTime),
    Date(Date),
    /// `bytea`; SQLite `BLOB`.
    Bytes(&'a [u8]),
    Bool(bool),
    Uuid(Uuid),
    /// A value of a type of the generic overload: the text of the type's output function with
    /// the fixed settings of the module docs. Also `infinity` / `-infinity` of a time or date.
    Other(&'a str),
}

/// The canonical text of a value (decision 0004-34 e). The input of the HMAC.
pub fn canonical_text(value: &SqlValue<'_>) -> String {
    match value {
        SqlValue::Text(s) | SqlValue::Other(s) => (*s).to_string(),
        SqlValue::Int(i) => i.to_string(),
        SqlValue::Numeric(n) => n.as_str().to_string(),
        SqlValue::Float4(f) => {
            if f.is_finite() {
                float_layout(
                    &format!("{:e}", f.abs()),
                    f.is_sign_negative(),
                    *f == 0.0,
                    6,
                )
            } else {
                float_special(f.is_nan(), *f > 0.0)
            }
        }
        SqlValue::Float8(f) => {
            if f.is_finite() {
                float_layout(
                    &format!("{:e}", f.abs()),
                    f.is_sign_negative(),
                    *f == 0.0,
                    15,
                )
            } else {
                float_special(f.is_nan(), *f > 0.0)
            }
        }
        SqlValue::Timestamptz(t) => {
            let t = t.to_offset(UtcOffset::UTC);
            timestamp_text(t.date(), t.time())
        }
        SqlValue::Timestamp(t) => timestamp_text(t.date(), t.time()),
        SqlValue::Date(d) => date_text(*d),
        SqlValue::Bytes(b) => hex::encode(b),
        SqlValue::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        SqlValue::Uuid(u) => u.hyphenated().to_string(),
    }
}

fn float_special(nan: bool, positive: bool) -> String {
    match (nan, positive) {
        (true, _) => "NaN",
        (false, true) => "Infinity",
        (false, false) => "-Infinity",
    }
    .to_string()
}

/// Lays out the shortest round-trip digits (`sci`, Rust's `{:e}` of the absolute value, for
/// example `1.5e-5`) as Postgres `float8out` / `float4out` do with `extra_float_digits >= 1`:
/// fixed notation for a decimal exponent in `[-4, fixed_below)`, else `d.ddde+XX` with at
/// least two exponent digits.
fn float_layout(sci: &str, negative: bool, zero: bool, fixed_below: i32) -> String {
    let sign = if negative { "-" } else { "" };
    if zero {
        return format!("{sign}0");
    }
    let (mant, exp) = sci.split_once('e').expect("`{:e}` writes an exponent");
    let exp: i32 = exp.parse().expect("`{:e}` writes a decimal exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    if (-4..fixed_below).contains(&exp) {
        let fixed = if exp >= 0 {
            let int_len = (exp + 1) as usize;
            if digits.len() <= int_len {
                format!("{digits}{}", "0".repeat(int_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..int_len], &digits[int_len..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        };
        format!("{sign}{fixed}")
    } else {
        let mant = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{mant}e{esign}{:02}", exp.abs())
    }
}

fn date_text(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// `YYYY-MM-DDTHH:MM:SS[.ffffff without trailing zeros]Z`. Postgres keeps microseconds, so
/// finer digits are cut.
fn timestamp_text(d: Date, t: Time) -> String {
    let mut out = format!(
        "{}T{:02}:{:02}:{:02}",
        date_text(d),
        t.hour(),
        t.minute(),
        t.second()
    );
    let micro = t.nanosecond() / 1000;
    if micro > 0 {
        let frac = format!("{micro:06}");
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
    out.push('Z');
    out
}

// ------------------------------------------------------------------------------ pseudonymizer

/// The pseudonym function of both engines. One instance is bound to the pseudonym key of one
/// database.
pub trait Pseudonymizer: Send + Sync {
    /// The pseudonym of a value. Constants and parameters of a statement are never passed here
    /// (decision 0004-24).
    fn pseudonym(&self, value: &SqlValue<'_>) -> Pseudonym;
}

/// [`Pseudonymizer`] with HMAC-SHA-256: `p_` plus the first 12 hex characters of
/// `HMAC-SHA-256(key, canonical_text(v))`.
#[derive(Clone, Debug)]
pub struct HmacPseudonymizer {
    key: PseudonymKey,
}

impl HmacPseudonymizer {
    pub fn new(key: PseudonymKey) -> Self {
        HmacPseudonymizer { key }
    }

    /// The pseudonym of a canonical text that the caller already made (for example the SQLite
    /// function, which gets the storage class of the value).
    pub fn pseudonym_of_canonical(&self, canonical: &str) -> Pseudonym {
        let mac = hmac_sha256(self.key.as_bytes(), canonical.as_bytes());
        let mut id = String::with_capacity(2 + PSEUDONYM_HEX_LEN);
        id.push_str("p_");
        id.push_str(&hex::encode(&mac[..PSEUDONYM_HEX_LEN / 2]));
        Pseudonym { id }
    }
}

impl Pseudonymizer for HmacPseudonymizer {
    fn pseudonym(&self, value: &SqlValue<'_>) -> Pseudonym {
        self.pseudonym_of_canonical(&canonical_text(value))
    }
}

// ---------------------------------------------------------------------------- masking rules

/// The hook for a change of the column classes of a database (`database_columns`, the
/// manifest's `data_classes`): a confirmation, a reset by the catalog sync, a new column.
/// The implementation rebuilds what depends on the classes (the lens views). Card K-1 calls it
/// after every change of classes; card Q-2 implements it for Postgres (`postgres.rs`), card Q-3
/// for SQLite (`sqlite.rs`). An implementation does nothing for a database of another engine,
/// so [`AllMaskingRules`] can call both.
#[async_trait]
pub trait MaskingRules: Send + Sync {
    /// The classes of the database `node` of `tenant` changed.
    async fn classes_changed(&self, tenant: TenantId, node: NodeId) -> Result<()>;
}

/// Calls each implementation in order and stops at the first error.
#[derive(Clone, Default)]
pub struct AllMaskingRules {
    rules: Vec<Arc<dyn MaskingRules>>,
}

impl AllMaskingRules {
    pub fn new(rules: Vec<Arc<dyn MaskingRules>>) -> Self {
        AllMaskingRules { rules }
    }
}

#[async_trait]
impl MaskingRules for AllMaskingRules {
    async fn classes_changed(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        for rules in &self.rules {
            rules.classes_changed(tenant, node).await?;
        }
        Ok(())
    }
}

/// One call of [`MaskingRules::classes_changed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClassesChanged {
    pub tenant: TenantId,
    pub node: NodeId,
}

/// The in-memory [`MaskingRules`] for tests: it records every call and rebuilds nothing.
#[derive(Debug, Default)]
pub struct RecordingMaskingRules {
    calls: Mutex<Vec<ClassesChanged>>,
}

impl RecordingMaskingRules {
    pub fn new() -> Self {
        Self::default()
    }

    /// The calls so far, oldest first.
    pub fn calls(&self) -> Vec<ClassesChanged> {
        self.calls.lock().expect("lock").clone()
    }

    /// Returns the calls so far and forgets them.
    pub fn take(&self) -> Vec<ClassesChanged> {
        std::mem::take(&mut *self.calls.lock().expect("lock"))
    }
}

#[async_trait]
impl MaskingRules for RecordingMaskingRules {
    async fn classes_changed(&self, tenant: TenantId, node: NodeId) -> Result<()> {
        self.calls
            .lock()
            .expect("lock")
            .push(ClassesChanged { tenant, node });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::format_description::well_known::Rfc3339;
    use time::macros::{datetime, format_description};

    /// The fixed test key 0x00..0x1f of spike S-1.
    fn test_key() -> PseudonymKey {
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        PseudonymKey::from_bytes(k)
    }

    /// "Pseudonym test vectors" of `ruum-server-wt/panel/step2-spike-s1.md`: the SQL output of
    /// `ruum_mask.canonical` and `ruum_mask.pseudonym` under hostile session settings
    /// (`TimeZone = Pacific/Chatham`, `DateStyle = SQL, DMY`, `extra_float_digits = -3`,
    /// `bytea_output = escape`, `IntervalStyle = sql_standard`), key 0x00..0x1f. The literals are
    /// `spikes/step2-roles/values.tsv`. Columns: type, literal, canonical text, pseudonym.
    const SPIKE_VECTORS: [(&str, &str, &str, &str); 29] = [
        (
            "text",
            "alice@example.com",
            "alice@example.com",
            "p_a59fc578d4cb",
        ),
        ("text", "Ünïcode ✓ 中文", "Ünïcode ✓ 中文", "p_c9cff20b05f9"),
        ("text", "", "", "p_d38b42096d80"),
        ("int8", "-42", "-42", "p_2156a1be4067"),
        (
            "int8",
            "9223372036854775807",
            "9223372036854775807",
            "p_250c40be2e1e",
        ),
        ("numeric", "1.50", "1.5", "p_254d5d122e4a"),
        ("numeric", "-0.000100", "-0.0001", "p_076d10355f90"),
        ("numeric", "100", "100", "p_59440f31cb67"),
        (
            "numeric",
            "123456789012345678901234567890.123400",
            "123456789012345678901234567890.1234",
            "p_32e11e873e69",
        ),
        ("float8", "0.1", "0.1", "p_c68a3e99cfa2"),
        ("float8", "1e21", "1e+21", "p_0e08c007c8e1"),
        ("float8", "1.5e-5", "1.5e-05", "p_8734ccb07ac8"),
        ("float8", "0.0001", "0.0001", "p_1f1dcdce1346"),
        ("float8", "-0", "-0", "p_7eb533b5b939"),
        (
            "float8",
            "3.141592653589793",
            "3.141592653589793",
            "p_2a2183e6d5f9",
        ),
        (
            "float8",
            "123456789012345.6",
            "123456789012345.6",
            "p_648eabc346b5",
        ),
        (
            "float8",
            "1234567890123456",
            "1.234567890123456e+15",
            "p_a6dbb62fb6fe",
        ),
        ("float8", "1e-320", "1e-320", "p_5ee18f53bf52"),
        ("float8", "Infinity", "Infinity", "p_4d1a00207d32"),
        ("float8", "NaN", "NaN", "p_7fb853b18b47"),
        (
            "timestamptz",
            "2026-11-07T12:34:56.789+02:00",
            "2026-11-07T10:34:56.789Z",
            "p_4f00da9579d9",
        ),
        (
            "timestamptz",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z",
            "p_2d5175fd30b8",
        ),
        (
            "timestamptz",
            "1999-12-31T23:59:59.000001-08:00",
            "2000-01-01T07:59:59.000001Z",
            "p_94cbd3d335ea",
        ),
        ("date", "2026-11-07", "2026-11-07", "p_86cf4c04cf1c"),
        ("bytea", "\\xDEADbeef00", "deadbeef00", "p_86de3b507e52"),
        ("bytea", "\\x", "", "p_d38b42096d80"),
        ("bool", "true", "true", "p_4476aeee13a6"),
        ("bool", "false", "false", "p_a290a0a8027b"),
        (
            "uuid",
            "0190F0E2-7C4A-7B3E-9F00-0123456789AB",
            "0190f0e2-7c4a-7b3e-9f00-0123456789ab",
            "p_8fc66ef9f72f",
        ),
    ];

    /// Reads a literal of `values.tsv` as the SQL `'<literal>'::<type>` does.
    fn check_vector(ty: &str, lit: &str, canonical: &str, pseudonym: &str) {
        let bytes;
        let value = match ty {
            "text" => SqlValue::Text(lit),
            "int8" => SqlValue::Int(lit.parse().unwrap()),
            "numeric" => SqlValue::Numeric(Numeric::parse(lit).unwrap()),
            "float8" => SqlValue::Float8(lit.parse().unwrap()),
            "timestamptz" => SqlValue::Timestamptz(OffsetDateTime::parse(lit, &Rfc3339).unwrap()),
            "date" => SqlValue::Date(
                Date::parse(lit, format_description!("[year]-[month]-[day]")).unwrap(),
            ),
            "bytea" => {
                bytes = hex::decode(lit.strip_prefix("\\x").unwrap()).unwrap();
                SqlValue::Bytes(&bytes)
            }
            "bool" => SqlValue::Bool(lit.parse().unwrap()),
            "uuid" => SqlValue::Uuid(Uuid::parse_str(lit).unwrap()),
            other => panic!("unknown type {other}"),
        };
        assert_eq!(
            canonical_text(&value),
            canonical,
            "canonical text of {ty} {lit:?}"
        );
        let p = HmacPseudonymizer::new(test_key());
        assert_eq!(
            p.pseudonym(&value).id(),
            pseudonym,
            "pseudonym of {ty} {lit:?}"
        );
        assert_eq!(p.pseudonym_of_canonical(canonical).id(), pseudonym);
    }

    #[test]
    fn rust_pseudonym_matches_the_sql_output_of_the_spike() {
        for (ty, lit, canonical, pseudonym) in SPIKE_VECTORS {
            check_vector(ty, lit, canonical, pseudonym);
        }
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex::encode(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 1: key 0x0b x 20.
        assert_eq!(
            hex::encode(hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Test case 6: a key longer than a block is hashed first.
        assert_eq!(
            hex::encode(hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn derived_key_matches_the_test_vector() {
        // HMAC-SHA-256(0x00..0x1f, 'db:0190f0e2-7c4a-7b3e-9f00-0123456789ab'), computed with
        // `openssl dgst -sha256 -mac HMAC`.
        let node: NodeId = "0190F0E2-7C4A-7B3E-9F00-0123456789AB".parse().unwrap();
        let derived = test_key().derive_for_database(node);
        assert_eq!(
            hex::encode(derived.as_bytes()),
            "788dad64979b9ef458ecaa8735adade8580b39e414c624a49493061f887680dc"
        );
        assert_eq!(PseudonymKey::for_database(&test_key(), Some(node)), derived);
        assert_eq!(PseudonymKey::for_database(&test_key(), None), test_key());

        // The same value gives another pseudonym through a draft branch's key.
        let v = SqlValue::Text("alice@example.com");
        assert_eq!(
            HmacPseudonymizer::new(derived).pseudonym(&v).id(),
            "p_9e3aec295442"
        );
        assert_eq!(
            HmacPseudonymizer::new(test_key()).pseudonym(&v).id(),
            "p_a59fc578d4cb"
        );
    }

    #[test]
    fn key_debug_hides_the_bytes() {
        assert_eq!(format!("{:?}", test_key()), "PseudonymKey(<hidden>)");
        assert!(PseudonymKey::from_slice(&[0; 31]).is_none());
        assert_eq!(
            PseudonymKey::from_slice(&[7; 32]).unwrap().as_bytes(),
            &[7; 32]
        );
    }

    #[test]
    fn canonical_numeric() {
        let c = |s: &str| Numeric::parse(s).map(|n| n.as_str().to_string());
        assert_eq!(c("1.50").as_deref(), Some("1.5"));
        assert_eq!(c("100").as_deref(), Some("100"));
        assert_eq!(c("100.000").as_deref(), Some("100"));
        assert_eq!(c("-0.000").as_deref(), Some("0"));
        assert_eq!(c("0").as_deref(), Some("0"));
        assert_eq!(c("007.10").as_deref(), Some("7.1"));
        assert_eq!(c("+3").as_deref(), Some("3"));
        assert_eq!(c("-.5").as_deref(), Some("-0.5"));
        assert_eq!(c("NaN").as_deref(), Some("NaN"));
        assert_eq!(c("Infinity").as_deref(), Some("Infinity"));
        assert_eq!(c("-Infinity").as_deref(), Some("-Infinity"));
        for bad in ["", "-", ".", "1e3", "1.2.3", "abc", " 1"] {
            assert_eq!(c(bad), None, "{bad:?}");
        }
    }

    /// float4 and float8 in the layout of `float4out` / `float8out` with
    /// `extra_float_digits = 1`, as PostgreSQL 17 printed them on the build server.
    #[test]
    fn canonical_floats_follow_postgres() {
        let f8 = |v: f64| canonical_text(&SqlValue::Float8(v));
        assert_eq!(f8(1e15), "1e+15");
        assert_eq!(f8(1e14), "100000000000000");
        assert_eq!(f8(999999999999999.0), "999999999999999");
        assert_eq!(f8(123456789012345678.0), "1.2345678901234568e+17");
        assert_eq!(f8(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(f8(0.0), "0");

        let f4 = |v: f32| canonical_text(&SqlValue::Float4(v));
        assert_eq!(f4(0.1), "0.1");
        assert_eq!(f4(1e6), "1e+06");
        assert_eq!(f4(123456.0), "123456");
        assert_eq!(f4(1e5), "100000");
        assert_eq!(f4(1.5e-5), "1.5e-05");
        assert_eq!(f4(0.0001), "0.0001");
        assert_eq!(f4(3.4028235e38), "3.4028235e+38");
        assert_eq!(f4(1e-45), "1e-45");
        assert_eq!(f4(-0.0), "-0");
        assert_eq!(f4(16777216.0), "1.6777216e+07");
        assert_eq!(f4(1234567.0), "1.234567e+06");
        assert_eq!(f4(f32::NAN), "NaN");
        assert_eq!(f4(f32::INFINITY), "Infinity");
    }

    #[test]
    fn canonical_times_bytes_and_others() {
        let ts = SqlValue::Timestamp(datetime!(2026-11-07 12:34:56.5));
        assert_eq!(canonical_text(&ts), "2026-11-07T12:34:56.5Z");
        // Nanoseconds beyond microseconds are cut, as in Postgres.
        let tz = SqlValue::Timestamptz(datetime!(2026-11-07 00:00:00.000000999 -01:00));
        assert_eq!(canonical_text(&tz), "2026-11-07T01:00:00Z");
        assert_eq!(canonical_text(&SqlValue::Bytes(&[0xAB, 0x01])), "ab01");
        assert_eq!(
            canonical_text(&SqlValue::Int(i64::MIN)),
            "-9223372036854775808"
        );
        assert_eq!(canonical_text(&SqlValue::Other("12:30:00")), "12:30:00");
        assert_eq!(canonical_text(&SqlValue::Other("infinity")), "infinity");
    }

    #[test]
    fn markers_serialize_as_the_contract_schemas() {
        assert_eq!(
            serde_json::to_value(HiddenValue::SECRET).unwrap(),
            json!({"$ruum": "hidden", "class": "secret"})
        );
        assert_eq!(
            serde_json::to_value(HiddenValue::PERSONAL).unwrap(),
            json!({"$ruum": "hidden", "class": "personal"})
        );
        let p = HmacPseudonymizer::new(test_key()).pseudonym(&SqlValue::Text("alice@example.com"));
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            json!({"$ruum": "pseudonym", "id": "p_a59fc578d4cb"})
        );
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"$ruum":"pseudonym","id":"p_a59fc578d4cb"}"#
        );
        let lit = LiteralValue {
            value: json!({"$ruum": "hidden", "class": "secret"}),
        };
        assert_eq!(
            serde_json::to_value(&lit).unwrap(),
            json!({"$ruum": "literal", "value": {"$ruum": "hidden", "class": "secret"}})
        );
        assert_eq!(
            serde_json::to_value(Marker::from(HiddenValue::SECRET)).unwrap(),
            json!({"$ruum": "hidden", "class": "secret"})
        );
    }

    #[test]
    fn markers_read_back_and_refuse_what_the_contract_refuses() {
        let m: Marker =
            serde_json::from_value(json!({"$ruum": "hidden", "class": "personal"})).unwrap();
        assert_eq!(m, Marker::Hidden(HiddenValue::PERSONAL));
        let p: Pseudonym =
            serde_json::from_value(json!({"$ruum": "pseudonym", "id": "p_8f3a91"})).unwrap();
        assert_eq!(p.id(), "p_8f3a91");
        let l: LiteralValue =
            serde_json::from_value(json!({"$ruum": "literal", "value": null})).unwrap();
        assert_eq!(l.value, serde_json::Value::Null);

        for bad in [
            json!({"$ruum": "hidden", "class": "normal"}),
            json!({"$ruum": "hidden"}),
            json!({"$ruum": "hidden", "class": "secret", "extra": 1}),
            json!({"$ruum": "pseudonym", "id": "p_ABCDEF"}),
            json!({"$ruum": "pseudonym", "id": "p_12345"}),
            json!({"$ruum": "pseudonym", "id": format!("p_{}", "a".repeat(33))}),
            json!({"$ruum": "pseudonym", "id": "x_123456"}),
            json!({"$ruum": "literal"}),
            json!({"$ruum": "other"}),
            json!({"class": "secret"}),
        ] {
            assert!(
                serde_json::from_value::<Marker>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        assert!(
            serde_json::from_value::<HiddenValue>(json!({"$ruum": "pseudonym", "id": "p_8f3a91"}))
                .is_err()
        );
        assert!(Pseudonym::parse(&format!("p_{}", "0".repeat(32))).is_some());
    }

    #[test]
    fn literal_cell_wraps_only_objects_with_the_marker_key() {
        let obj = json!({"$ruum": "hidden", "class": "secret"});
        assert_eq!(
            LiteralValue::cell(obj.clone()),
            json!({"$ruum": "literal", "value": obj})
        );
        for plain in [
            json!({"a": 1}),
            json!([{"$ruum": 1}]),
            json!("x"),
            json!(null),
            json!(1.5),
        ] {
            assert_eq!(LiteralValue::cell(plain.clone()), plain);
        }
    }

    #[tokio::test]
    async fn recording_masking_rules_records_calls() {
        let rec = Arc::new(RecordingMaskingRules::new());
        let (t, n1, n2) = (TenantId::new(), NodeId::new(), NodeId::new());
        let rules: Arc<dyn MaskingRules> = rec.clone();
        rules.classes_changed(t, n1).await.unwrap();

        // Both engines' implementations behind one hook.
        let other = Arc::new(RecordingMaskingRules::new());
        let all = AllMaskingRules::new(vec![rec.clone(), other.clone()]);
        all.classes_changed(t, n2).await.unwrap();

        assert_eq!(
            rec.calls(),
            vec![
                ClassesChanged {
                    tenant: t,
                    node: n1
                },
                ClassesChanged {
                    tenant: t,
                    node: n2
                }
            ]
        );
        assert_eq!(
            other.take(),
            vec![ClassesChanged {
                tenant: t,
                node: n2
            }]
        );
        assert!(other.calls().is_empty());
    }
}
