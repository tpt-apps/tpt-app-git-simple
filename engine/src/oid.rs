//! A git object id: 20 raw bytes (SHA-1), with hex conversion for refs,
//! loose-object paths, and display.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid(pub [u8; 20]);

impl Oid {
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim();
        if hex.len() != 40 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut raw = [0u8; 20];
        for (i, byte) in raw.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(Oid(raw))
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(40);
        for b in &self.0 {
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
            s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
        }
        s
    }

    /// Short display form (first 7 hex chars), the convention most git UIs
    /// use for commit lists.
    pub fn short(&self) -> String {
        let hex = self.to_hex();
        hex[..7].to_string()
    }

    /// Loose-object path under `.git/objects/`: `ab/cdef…`.
    pub fn loose_path(&self) -> String {
        let hex = self.to_hex();
        format!("{}/{}", &hex[..2], &hex[2..])
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let hex = "0123456789abcdef0123456789abcdef01234567";
        let oid = Oid::from_hex(hex).unwrap();
        assert_eq!(oid.to_hex(), hex);
        assert_eq!(oid.short(), "0123456");
        assert_eq!(oid.loose_path(), "01/23456789abcdef0123456789abcdef01234567");
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(Oid::from_hex("nothex").is_none());
        assert!(Oid::from_hex(&"a".repeat(41)).is_none());
        assert!(Oid::from_hex(&"g".repeat(40)).is_none());
    }
}
