//! Strong versioned validators for local kernels and API batches.
//! Authentication and durable external-effect transactions belong to the host.
pub mod api;
pub mod journal;
pub mod kernel;
pub mod lab;
#[cfg(feature = "openai-http")]
pub mod openai_http;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;

pub const MAX_MEMBERS: usize = 1024;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum Error {
    MissingPrecondition,
    StaleSnapshot,
    StaleRevision,
    OperationConflict,
    Invalid { detail: String },
    Storage,
    Internal,
}
impl Error {
    pub fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            detail: detail.into(),
        }
    }
    pub fn http_status(&self) -> u16 {
        match self {
            Self::MissingPrecondition => 428,
            Self::StaleSnapshot | Self::StaleRevision => 412,
            Self::OperationConflict => 409,
            Self::Invalid { .. } => 400,
            Self::Storage => 503,
            Self::Internal => 500,
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// The wire format is a single quoted strong validator, never W/ or '*'.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Etag(String);
impl Etag {
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() != 73
            || !s.starts_with("\"sha256-")
            || !s.ends_with('"')
            || !s.as_bytes()[8..72]
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        {
            return Err(Error::invalid(
                "expected one quoted sha256-<64 lowercase hex> strong ETag",
            ));
        }
        Ok(Self(s.to_owned()))
    }
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(format!("\"sha256-{:x}\"", Sha256::digest(bytes)))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Strict application contract: exactly one strong If-Match, no wildcard/list.
    pub fn require(&self, expected: Option<&str>) -> Result<()> {
        let expected = Etag::parse(expected.ok_or(Error::MissingPrecondition)?)?;
        if expected == *self {
            Ok(())
        } else {
            Err(Error::StaleSnapshot)
        }
    }
}
impl fmt::Display for Etag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl<'de> Deserialize<'de> for Etag {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

/// Version-1 encoding: sorted UTF-8 object keys, array order retained, serde
/// JSON scalar encoding. This is deliberately NOT RFC 8785/JCS.
pub fn canonical<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    fn write(v: &Value, out: &mut Vec<u8>) -> Result<()> {
        match v {
            Value::Object(map) => {
                out.push(b'{');
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort_unstable();
                for (i, key) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    serde_json::to_writer(&mut *out, key).map_err(|_| Error::Internal)?;
                    out.push(b':');
                    write(&map[key], out)?;
                }
                out.push(b'}');
            }
            Value::Array(items) => {
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    write(item, out)?;
                }
                out.push(b']');
            }
            scalar => serde_json::to_writer(out, scalar).map_err(|_| Error::Internal)?,
        }
        Ok(())
    }
    let value = serde_json::to_value(value).map_err(|_| Error::invalid("not a JSON value"))?;
    let mut out = Vec::new();
    write(&value, &mut out)?;
    Ok(out)
}
pub fn identity<T: Serialize + ?Sized>(value: &T) -> Result<Etag> {
    Ok(Etag::of_bytes(&canonical(value)?))
}

/// Byte-compatible with Q-Calculator's existing tuple hash, PROVIDED jobs are
/// its already-normalized typed jobs serialized by the same serde contract.
/// Does not normalize defaults, validate circuits or convert arbitrary JSON.
pub fn qcalculator_v1_etag<T: Serialize>(
    executable_sha256: &str,
    normalized_jobs: &[T],
) -> Result<Etag> {
    check_sha256(executable_sha256)?;
    let bytes = serde_json::to_vec(&("q-calculator.batch.v1", executable_sha256, normalized_jobs))
        .map_err(|_| Error::invalid("legacy serialization"))?;
    Ok(Etag::of_bytes(&bytes))
}
pub fn check_sha256(s: &str) -> Result<()> {
    if s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(Error::invalid("expected SHA-256 lowercase hex"))
    }
}
pub fn check_id(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 128 || s.chars().any(char::is_control) {
        Err(Error::invalid(
            "identity must contain 1..128 bytes without controls",
        ))
    } else {
        Ok(())
    }
}

/// The validator covers the schema, accepted input and complete result payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Report<T> {
    pub schema: String,
    pub input_etag: Etag,
    pub etag: Etag,
    pub payload: T,
}
impl<T: Serialize> Report<T> {
    pub fn new(input_etag: Etag, payload: T) -> Result<Self> {
        let schema = "amqic.etag.result.v1".to_owned();
        let etag = identity(&(&schema, &input_etag, &payload))?;
        Ok(Self {
            schema,
            input_etag,
            etag,
            payload,
        })
    }
    pub fn verify(&self, expected_input: &Etag) -> Result<()> {
        if self.schema != "amqic.etag.result.v1"
            || self.input_etag != *expected_input
            || self.etag != identity(&(&self.schema, &self.input_etag, &self.payload))?
        {
            return Err(Error::StaleSnapshot);
        }
        Ok(())
    }
}
