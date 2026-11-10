//! Parameters and result cells: [`Value`] to and from the binary format of PostgreSQL.
//!
//! sqlx sends parameters and receives results in the binary format. The engine decodes the
//! common types by their OID itself (also `numeric`, which needs no extra crate here), and
//! refuses a result type it does not know (the caller casts it to `text`).

use crate::error::EngineError;
use crate::model::Value;
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgArgumentBuffer, PgArguments, PgTypeInfo, PgValueRef};
use sqlx::{Arguments, Encode, Postgres, Type, ValueRef};
use time::{Date, Duration as TimeDuration, PrimitiveDateTime, Time};

pub(crate) mod oid {
    pub const BOOL: u32 = 16;
    pub const BYTEA: u32 = 17;
    pub const CHAR: u32 = 18;
    pub const NAME: u32 = 19;
    pub const INT8: u32 = 20;
    pub const INT2: u32 = 21;
    pub const INT4: u32 = 23;
    pub const TEXT: u32 = 25;
    pub const OID: u32 = 26;
    pub const JSON: u32 = 114;
    pub const FLOAT4: u32 = 700;
    pub const FLOAT8: u32 = 701;
    pub const UNKNOWN: u32 = 705;
    pub const BPCHAR: u32 = 1042;
    pub const VARCHAR: u32 = 1043;
    pub const DATE: u32 = 1082;
    pub const TIME: u32 = 1083;
    pub const TIMESTAMP: u32 = 1114;
    pub const TIMESTAMPTZ: u32 = 1184;
    pub const NUMERIC: u32 = 1700;
    pub const UUID: u32 = 2950;
    pub const JSONB: u32 = 3802;
}

/// What the engine knows of a result type besides its OID (from `pg_type`).
#[derive(Debug, Clone)]
pub(crate) struct TypeFacts {
    /// `format_type(oid, NULL)`: `integer`, `text`, `timestamp with time zone`, ...
    pub name: String,
    /// `typtype = 'e'` (an enum: its binary form is the label) or a string-category type such
    /// as `citext` (its binary form is the text).
    pub text_like: bool,
    /// The element type of an array type (`typelem` when `typcategory = 'A'`).
    pub element: Option<u32>,
}

/// The year 2000 (the epoch of PostgreSQL dates and timestamps).
fn pg_epoch_date() -> Date {
    Date::from_calendar_date(2000, time::Month::January, 1).expect("valid date")
}

/// A NULL parameter of no type: the server infers its type from the statement.
struct UntypedNull;

impl Type<Postgres> for UntypedNull {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(Oid(0))
    }
    fn compatible(_: &PgTypeInfo) -> bool {
        true
    }
}

impl Encode<'_, Postgres> for UntypedNull {
    fn encode_by_ref(&self, _: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Ok(IsNull::Yes)
    }
}

/// A `numeric` parameter in the binary format.
struct NumericParam(Vec<u8>);

impl Type<Postgres> for NumericParam {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_oid(Oid(oid::NUMERIC))
    }
}

impl Encode<'_, Postgres> for NumericParam {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        buf.extend_from_slice(&self.0);
        Ok(IsNull::No)
    }
}

fn bind_err(e: BoxDynError) -> EngineError {
    EngineError::Internal(format!("cannot bind a parameter: {e}"))
}

/// The arguments of an untrusted statement.
pub(crate) fn arguments(params: &[Value]) -> Result<PgArguments, EngineError> {
    let mut args = PgArguments::default();
    for value in params {
        match value {
            Value::Null => args.add(UntypedNull),
            Value::Bool(b) => args.add(*b),
            Value::Int(i) => args.add(*i),
            Value::Float(f) => args.add(*f),
            Value::Numeric(s) => args.add(NumericParam(encode_numeric(s).ok_or_else(|| {
                EngineError::Sql {
                    code: "22P02".to_string(),
                    message: "invalid input syntax for type numeric".to_string(),
                    in_routine: false,
                }
            })?)),
            Value::Text(s) => args.add(s.clone()),
            Value::Bytes(b) => args.add(b.clone()),
            Value::Json(j) => args.add(sqlx::types::Json(j.clone())),
            Value::Timestamp(t) => args.add(*t),
            Value::Date(d) => args.add(*d),
            Value::Uuid(u) => args.add(*u),
            Value::Array(items) => add_array(&mut args, items)?,
        }
        .map_err(bind_err)?;
    }
    Ok(args)
}

/// An array parameter of one element kind (integers, floats, texts, booleans or UUIDs).
fn add_array(
    args: &mut PgArguments,
    items: &[Value],
) -> Result<Result<(), BoxDynError>, EngineError> {
    let unsupported =
        || EngineError::Unsupported("array parameters of mixed or nested values".to_string());
    let first = items.iter().find(|v| !matches!(v, Value::Null));
    macro_rules! collect {
        ($variant:ident, $ty:ty) => {{
            let mut out: Vec<Option<$ty>> = Vec::with_capacity(items.len());
            for v in items {
                match v {
                    Value::Null => out.push(None),
                    Value::$variant(x) => out.push(Some(x.clone())),
                    _ => return Err(unsupported()),
                }
            }
            Ok(args.add(out))
        }};
    }
    match first {
        None | Some(Value::Text(_)) => collect!(Text, String),
        Some(Value::Int(_)) => collect!(Int, i64),
        Some(Value::Float(_)) => collect!(Float, f64),
        Some(Value::Bool(_)) => collect!(Bool, bool),
        Some(Value::Uuid(_)) => collect!(Uuid, uuid::Uuid),
        Some(_) => Err(unsupported()),
    }
}

/// The binary form of a `numeric` from its decimal text (`-12.340`, `1e3` is refused).
pub(crate) fn encode_numeric(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut out = Vec::new();
    let header = |ndigits: i16, weight: i16, sign: u16, dscale: i16, out: &mut Vec<u8>| {
        out.extend_from_slice(&ndigits.to_be_bytes());
        out.extend_from_slice(&weight.to_be_bytes());
        out.extend_from_slice(&sign.to_be_bytes());
        out.extend_from_slice(&dscale.to_be_bytes());
    };
    match digits.to_ascii_lowercase().as_str() {
        "nan" => {
            header(0, 0, 0xC000, 0, &mut out);
            return Some(out);
        }
        "infinity" | "inf" => {
            header(0, 0, if negative { 0xF000 } else { 0xD000 }, 0, &mut out);
            return Some(out);
        }
        _ => {}
    }
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part
        .bytes()
        .chain(frac_part.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let dscale = i16::try_from(frac_part.len()).ok()?;
    // Pad the integer part on the left and the fraction on the right to groups of 4 digits.
    let int_trim = int_part.trim_start_matches('0');
    let int_pad = (4 - int_trim.len() % 4) % 4;
    let frac_pad = (4 - frac_part.len() % 4) % 4;
    let all: String = "0".repeat(int_pad) + int_trim + frac_part + &"0".repeat(frac_pad);
    let groups: Vec<i16> = all
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).ok().and_then(|s| s.parse().ok()))
        .collect::<Option<_>>()?;
    let int_groups = (int_trim.len() + int_pad) / 4;
    let mut weight = int_groups as i32 - 1;
    // Drop leading and trailing zero groups.
    let mut start = 0;
    while start < groups.len() && groups[start] == 0 {
        start += 1;
        weight -= 1;
    }
    let mut end = groups.len();
    while end > start && groups[end - 1] == 0 {
        end -= 1;
    }
    let groups = &groups[start..end];
    let sign = if negative && !groups.is_empty() {
        0x4000
    } else {
        0
    };
    let weight = if groups.is_empty() { 0 } else { weight };
    header(
        i16::try_from(groups.len()).ok()?,
        i16::try_from(weight).ok()?,
        sign,
        dscale,
        &mut out,
    );
    for g in groups {
        out.extend_from_slice(&g.to_be_bytes());
    }
    Some(out)
}

/// The decimal text of a binary `numeric` (as `numeric_out` writes it).
pub(crate) fn decode_numeric(b: &[u8]) -> Option<String> {
    let rd =
        |i: usize| -> Option<i16> { Some(i16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?)) };
    let ndigits = rd(0)? as usize;
    let weight = rd(2)? as i32;
    let sign = rd(4)? as u16;
    let dscale = rd(6)?.max(0) as usize;
    match sign {
        0xC000 => return Some("NaN".to_string()),
        0xD000 => return Some("Infinity".to_string()),
        0xF000 => return Some("-Infinity".to_string()),
        _ => {}
    }
    let mut digits = Vec::with_capacity(ndigits);
    for k in 0..ndigits {
        digits.push(rd(8 + 2 * k)? as i32);
    }
    let group = |idx: i32| -> i32 {
        if idx >= 0 && (idx as usize) < digits.len() {
            digits[idx as usize]
        } else {
            0
        }
    };
    let mut int = String::new();
    if weight >= 0 {
        for w in 0..=weight {
            let g = group(w);
            if w == 0 {
                int.push_str(&g.to_string());
            } else {
                int.push_str(&format!("{g:04}"));
            }
        }
    } else {
        int.push('0');
    }
    let mut frac = String::new();
    let mut idx = weight + 1;
    while frac.len() < dscale {
        frac.push_str(&format!("{:04}", group(idx)));
        idx += 1;
    }
    frac.truncate(dscale);
    let negative = sign == 0x4000;
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&int);
    if dscale > 0 {
        out.push('.');
        out.push_str(&frac);
    }
    Some(out)
}

fn be_i16(b: &[u8]) -> Option<i16> {
    Some(i16::from_be_bytes(b.get(..2)?.try_into().ok()?))
}
fn be_i32(b: &[u8]) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}
fn be_i64(b: &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(..8)?.try_into().ok()?))
}

fn timestamp_value(micros: i64) -> Value {
    match micros {
        i64::MAX => Value::Text("infinity".to_string()),
        i64::MIN => Value::Text("-infinity".to_string()),
        _ => {
            let base = PrimitiveDateTime::new(pg_epoch_date(), Time::MIDNIGHT).assume_utc();
            match base.checked_add(TimeDuration::microseconds(micros)) {
                Some(t) => Value::Timestamp(t),
                None => Value::Text("out of range".to_string()),
            }
        }
    }
}

fn time_text(micros: i64) -> String {
    let secs = micros / 1_000_000;
    let frac = micros % 1_000_000;
    let mut s = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    if frac > 0 {
        s.push('.');
        s.push_str(format!("{frac:06}").trim_end_matches('0'));
    }
    s
}

/// Decodes a binary cell of a known type OID. `None` for a type the engine does not decode.
pub(crate) fn decode(
    type_oid: u32,
    bytes: &[u8],
    facts: &dyn Fn(u32) -> Option<TypeFacts>,
) -> Result<Option<Value>, EngineError> {
    let bad = || EngineError::Internal(format!("cannot decode a value of type {type_oid}"));
    let text = |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|_| bad());
    Ok(Some(match type_oid {
        oid::BOOL => Value::Bool(bytes.first().copied().ok_or_else(bad)? != 0),
        oid::INT2 => Value::Int(be_i16(bytes).ok_or_else(bad)?.into()),
        oid::INT4 => Value::Int(be_i32(bytes).ok_or_else(bad)?.into()),
        oid::INT8 => Value::Int(be_i64(bytes).ok_or_else(bad)?),
        oid::OID => Value::Int(
            u32::from_be_bytes(
                bytes
                    .get(..4)
                    .ok_or_else(bad)?
                    .try_into()
                    .map_err(|_| bad())?,
            )
            .into(),
        ),
        oid::FLOAT4 => Value::Float(
            f32::from_be_bytes(
                bytes
                    .get(..4)
                    .ok_or_else(bad)?
                    .try_into()
                    .map_err(|_| bad())?,
            )
            .into(),
        ),
        oid::FLOAT8 => Value::Float(f64::from_be_bytes(
            bytes
                .get(..8)
                .ok_or_else(bad)?
                .try_into()
                .map_err(|_| bad())?,
        )),
        oid::NUMERIC => Value::Numeric(decode_numeric(bytes).ok_or_else(bad)?),
        oid::TEXT | oid::VARCHAR | oid::BPCHAR | oid::NAME | oid::UNKNOWN | oid::CHAR => {
            Value::Text(text(bytes)?)
        }
        oid::BYTEA => Value::Bytes(bytes.to_vec()),
        oid::JSON => Value::Json(serde_json::from_slice(bytes).map_err(|_| bad())?),
        oid::JSONB => {
            let body = bytes.get(1..).ok_or_else(bad)?;
            Value::Json(serde_json::from_slice(body).map_err(|_| bad())?)
        }
        oid::UUID => Value::Uuid(uuid::Uuid::from_slice(bytes).map_err(|_| bad())?),
        oid::DATE => match be_i32(bytes).ok_or_else(bad)? {
            i32::MAX => Value::Text("infinity".to_string()),
            i32::MIN => Value::Text("-infinity".to_string()),
            days => match pg_epoch_date().checked_add(TimeDuration::days(days.into())) {
                Some(d) => Value::Date(d),
                None => return Ok(None),
            },
        },
        oid::TIMESTAMP => timestamp_value(be_i64(bytes).ok_or_else(bad)?),
        oid::TIMESTAMPTZ => timestamp_value(be_i64(bytes).ok_or_else(bad)?),
        oid::TIME => Value::Text(time_text(be_i64(bytes).ok_or_else(bad)?)),
        other => match facts(other) {
            Some(f) if f.text_like => Value::Text(text(bytes)?),
            Some(TypeFacts {
                element: Some(_), ..
            }) => match decode_array(bytes, facts)? {
                Some(v) => v,
                None => return Ok(None),
            },
            _ => return Ok(None),
        },
    }))
}

/// A binary array (any number of dimensions, flattened in order into nested `Array`s).
fn decode_array(
    bytes: &[u8],
    facts: &dyn Fn(u32) -> Option<TypeFacts>,
) -> Result<Option<Value>, EngineError> {
    let bad = || EngineError::Internal("cannot decode an array value".to_string());
    let ndim = be_i32(bytes).ok_or_else(bad)?.max(0) as usize;
    let elem = u32::from_be_bytes(
        bytes
            .get(8..12)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?,
    );
    let mut dims = Vec::with_capacity(ndim);
    let mut pos = 12;
    for _ in 0..ndim {
        dims.push(be_i32(&bytes[pos..]).ok_or_else(bad)?.max(0) as usize);
        pos += 8;
    }
    let total: usize = if ndim == 0 { 0 } else { dims.iter().product() };
    let mut flat = Vec::with_capacity(total);
    for _ in 0..total {
        let len = be_i32(bytes.get(pos..).ok_or_else(bad)?).ok_or_else(bad)?;
        pos += 4;
        if len < 0 {
            flat.push(Value::Null);
            continue;
        }
        let len = len as usize;
        let cell = bytes.get(pos..pos + len).ok_or_else(bad)?;
        pos += len;
        match decode(elem, cell, facts)? {
            Some(v) => flat.push(v),
            None => return Ok(None),
        }
    }
    fn nest(flat: &mut std::vec::IntoIter<Value>, dims: &[usize]) -> Value {
        match dims {
            [] => Value::Array(Vec::new()),
            [n] => Value::Array(flat.by_ref().take(*n).collect()),
            [n, rest @ ..] => Value::Array((0..*n).map(|_| nest(flat, rest)).collect()),
        }
    }
    Ok(Some(nest(&mut flat.into_iter(), &dims)))
}

/// Decodes one cell of a row.
pub(crate) fn cell(
    value: PgValueRef<'_>,
    type_oid: u32,
    facts: &dyn Fn(u32) -> Option<TypeFacts>,
) -> Result<Value, EngineError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let bytes = value
        .as_bytes()
        .map_err(|e| EngineError::Internal(e.to_string()))?;
    match decode(type_oid, bytes, facts)? {
        Some(v) => Ok(v),
        None => {
            let name = facts(type_oid)
                .map(|f| f.name)
                .unwrap_or_else(|| type_oid.to_string());
            Err(EngineError::Unsupported(format!(
                "result values of type {name}: cast the column to text"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_round_trips_through_the_binary_form() {
        for text in [
            "0",
            "1",
            "-1",
            "1.50",
            "-0.000100",
            "100",
            "10000",
            "123456789012345678901234567890.123400",
            "0.0001",
            "99999999.9999",
            "-12.3",
        ] {
            let bin = encode_numeric(text).unwrap();
            assert_eq!(decode_numeric(&bin).unwrap(), text, "{text}");
        }
        assert_eq!(
            decode_numeric(&encode_numeric("NaN").unwrap()).unwrap(),
            "NaN"
        );
        assert_eq!(decode_numeric(&encode_numeric("+5").unwrap()).unwrap(), "5");
        assert_eq!(
            decode_numeric(&encode_numeric("007.5").unwrap()).unwrap(),
            "7.5"
        );
        assert!(encode_numeric("1e3").is_none());
        assert!(encode_numeric("").is_none());
        assert!(encode_numeric("abc").is_none());
    }

    #[test]
    fn times_decode_from_microseconds() {
        assert_eq!(time_text(0), "00:00:00");
        assert_eq!(
            time_text((12 * 3600 + 34 * 60 + 56) * 1_000_000 + 500_000),
            "12:34:56.5"
        );
        match timestamp_value(0) {
            Value::Timestamp(t) => assert_eq!(t.year(), 2000),
            other => panic!("{other:?}"),
        }
        assert_eq!(timestamp_value(i64::MAX), Value::Text("infinity".into()));
    }
}
