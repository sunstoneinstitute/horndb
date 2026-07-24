//! Human-string unit newtypes shared by config files and (later) URL params.
//!
//! `ByteSize` accepts a raw integer byte count or an IEC binary-unit suffix
//! (`KiB`/`MiB`/`GiB`/`TiB`, case-insensitive). `HumanDuration` accepts an
//! integer with an `ms`/`s`/`m`/`h` suffix. One grammar, no decimal-vs-binary
//! ambiguity (SPEC-26 S2 / Risks: pin one grammar).

use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(test)]
mod byte_size_tests {
    use super::*;

    #[test]
    fn parses_raw_bytes() {
        assert_eq!("1024".parse::<ByteSize>().unwrap(), ByteSize(1024));
        assert_eq!("0".parse::<ByteSize>().unwrap(), ByteSize(0));
    }

    #[test]
    fn parses_iec_suffixes() {
        assert_eq!(
            "2GiB".parse::<ByteSize>().unwrap(),
            ByteSize(2 * 1024 * 1024 * 1024)
        );
        assert_eq!(
            "512MiB".parse::<ByteSize>().unwrap(),
            ByteSize(512 * 1024 * 1024)
        );
        assert_eq!("1kib".parse::<ByteSize>().unwrap(), ByteSize(1024)); // case-insensitive
        assert_eq!(
            " 4 TiB ".parse::<ByteSize>().unwrap(),
            ByteSize(4 * 1024u64.pow(4))
        ); // trimmed + inner space
    }

    #[test]
    fn rejects_garbage() {
        assert!("2GB".parse::<ByteSize>().is_err()); // decimal units not accepted
        assert!("abc".parse::<ByteSize>().is_err());
        assert!("".parse::<ByteSize>().is_err());
        assert!("-5".parse::<ByteSize>().is_err());
    }
}

/// A byte count parsed from a raw integer or an IEC binary suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub u64);

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.is_empty() {
            return Err("empty byte size".to_string());
        }
        // Split leading digits from an optional unit suffix; allow an inner space.
        let digits_end = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        if digits_end == 0 {
            return Err(format!("no leading number in byte size {s:?}"));
        }
        let num: u64 = t[..digits_end]
            .parse()
            .map_err(|_| format!("invalid number in byte size {s:?}"))?;
        let unit = t[digits_end..].trim().to_ascii_lowercase();
        let mult: u64 = match unit.as_str() {
            "" | "b" => 1,
            "kib" => 1024,
            "mib" => 1024 * 1024,
            "gib" => 1024 * 1024 * 1024,
            "tib" => 1024u64.pow(4),
            other => {
                return Err(format!(
                    "unknown byte-size unit {other:?} (use B/KiB/MiB/GiB/TiB)"
                ))
            }
        };
        num.checked_mul(mult)
            .map(ByteSize)
            .ok_or_else(|| format!("byte size {s:?} overflows u64"))
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Accept either an integer (raw bytes) or a string ("2GiB").
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Int(u64),
            Str(String),
        }
        match Repr::deserialize(d)? {
            Repr::Int(n) => Ok(ByteSize(n)),
            Repr::Str(s) => s.parse().map_err(serde::de::Error::custom),
        }
    }
}
