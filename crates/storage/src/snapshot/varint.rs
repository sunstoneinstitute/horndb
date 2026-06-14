//! VByte (unsigned LEB128) and zigzag integer coding for the snapshot format.

use std::io::{self, Read, Write};

/// Write `value` as unsigned LEB128.
pub fn write_uvarint<W: Write>(w: &mut W, mut value: u64) -> io::Result<()> {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        w.write_all(&[byte])?;
        if value == 0 {
            return Ok(());
        }
    }
}

/// Read an unsigned LEB128 value.
pub fn read_uvarint<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        let mut buf = [0u8; 1];
        r.read_exact(&mut buf)?;
        let byte = buf[0];
        if shift >= 64 || (shift == 63 && (byte & 0x7f) > 1) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "uvarint overflows u64",
            ));
        }
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

/// Zigzag-encode an i32 into a u64 suitable for `write_uvarint`.
pub fn zigzag_encode(value: i32) -> u64 {
    ((value << 1) ^ (value >> 31)) as u32 as u64
}

/// Inverse of `zigzag_encode`.
pub fn zigzag_decode(value: u64) -> i32 {
    let v = value as u32;
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip_u(v: u64) -> u64 {
        let mut buf = Vec::new();
        write_uvarint(&mut buf, v).unwrap();
        read_uvarint(&mut &buf[..]).unwrap()
    }

    #[test]
    fn uvarint_round_trips_boundaries() {
        for v in [0u64, 1, 127, 128, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            assert_eq!(round_trip_u(v), v, "u {v}");
        }
    }

    #[test]
    fn uvarint_is_minimal_width() {
        let mut buf = Vec::new();
        write_uvarint(&mut buf, 127).unwrap();
        assert_eq!(buf.len(), 1);
        buf.clear();
        write_uvarint(&mut buf, 128).unwrap();
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn zigzag_round_trips() {
        for v in [0i32, -1, 1, i32::MIN, i32::MAX, -42, 42] {
            assert_eq!(zigzag_decode(zigzag_encode(v)), v, "zz {v}");
        }
    }

    #[test]
    fn truncated_uvarint_errors() {
        let buf = [0x80u8]; // continuation bit set, no following byte
        assert!(read_uvarint(&mut &buf[..]).is_err());
    }
}
