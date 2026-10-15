//! ULID-style identifiers: 26 Crockford base32 chars —
//! 48-bit millisecond timestamp + 80 bits of OS entropy, time-sortable.
//!
//! Prefixed per design (`ch_01J...`, `char_01J...`) so ids are self-describing
//! in links like `[[ch_01J...]]`.

use std::fmt;

/// Crockford base32 alphabet (no I, L, O, U).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// An OCA id: `<prefix><26-char ULID>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(String);

impl Id {
    /// Build an id from a prefix and a raw (unprefixed) ULID string.
    pub fn new(prefix: &str, ulid: impl Into<String>) -> Self {
        Self(format!("{prefix}_{}", ulid.into()))
    }

    /// Borrow the full id string, e.g. `ch_01J5ZQ3M8...`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The registered prefix without trailing underscore (`ch`, `char`, ...).
    pub fn prefix(&self) -> &str {
        match self.0.find('_') {
            Some(i) => &self.0[..i],
            None => "",
        }
    }

    /// The 26-char ULID body.
    pub fn ulid(&self) -> &str {
        match self.0.find('_') {
            Some(i) => &self.0[i + 1..],
            None => &self.0,
        }
    }

    /// Structural validation: known prefix shape + exactly 26 Crockford chars.
    pub fn is_valid(&self) -> bool {
        let u = self.ulid();
        u.len() == 26 && !self.prefix().is_empty() && u.bytes().all(|b| CROCKFORD.contains(&b))
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Id {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Generate a fresh id: `<prefix><ULID>` with current-time prefix so ids sort
/// chronologically.
pub fn new_id(prefix: &str) -> Id {
    Id::new(prefix, ulid_now())
}

/// A fresh 26-char ULID (48-bit ms timestamp || 80 random bits), monotonic
/// within a millisecond: repeated ids in the same ms increment the random
/// component so generation order == sort order (the ULID spec's "monotonic
/// mode"), and a backwards clock can never produce a smaller id.
fn ulid_now() -> String {
    let (millis, rand) = monotonic_parts();

    let mut out = String::with_capacity(26);
    // 48 timestamp bits -> 10 base32 chars (most significant first).
    for shift in (0..10).rev() {
        out.push(encode_char(((millis >> (shift * 5)) & 0x1f) as usize));
    }
    // 80 random bits -> 16 base32 chars.
    let mut acc: u64 = 0;
    let mut bits = 0;
    for byte in rand {
        acc = (acc << 8) | u64::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(encode_char(((acc >> bits) & 0x1f) as usize));
        }
    }
    debug_assert_eq!(out.len(), 26);
    out
}

fn monotonic_parts() -> (u64, [u8; 10]) {
    use std::sync::{Mutex, OnceLock};
    static MONO: OnceLock<Mutex<(u64, [u8; 10])>> = OnceLock::new();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let mut guard = MONO
        .get_or_init(|| Mutex::new((now, fresh_random())))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (last_ms, last_rand) = &mut *guard;
    if now > *last_ms {
        *last_ms = now;
        *last_rand = fresh_random();
    } else {
        // Same millisecond (or clock stepped back): keep time, +1 the random.
        increment(last_rand);
    }
    (*last_ms, *last_rand)
}

fn fresh_random() -> [u8; 10] {
    let mut rand = [0u8; 10];
    // OS entropy; failure here means the platform has no randomness source,
    // which no supported OCA target does.
    getrandom::fill(&mut rand).expect("OS entropy source unavailable");
    rand
}

/// Big-endian +1 with carry; on full wraparound (astronomically unlikely)
/// reseed from entropy so we never emit an id we already emitted.
fn increment(bytes: &mut [u8; 10]) {
    for b in bytes.iter_mut().rev() {
        if *b < 0xff {
            *b += 1;
            return;
        }
        *b = 0;
    }
    *bytes = fresh_random();
}

fn encode_char(v: usize) -> char {
    CROCKFORD[v] as char
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn new_id_shape_and_validity() {
        let id = new_id("ch");
        assert_eq!(id.prefix(), "ch");
        assert_eq!(id.ulid().len(), 26);
        assert!(id.is_valid(), "expected a valid id, got {id}");
        assert!(id.to_string().starts_with("ch_"));
    }

    #[test]
    fn ids_are_unique() {
        let set: HashSet<String> = (0..200).map(|_| new_id("ch").to_string()).collect();
        assert_eq!(set.len(), 200);
    }

    #[test]
    fn invalid_ids_rejected() {
        assert!(!Id::from("ch_short").is_valid());
        assert!(!Id::from("ch_0123456789ABCDEFGHIJKLMNO").is_valid()); // 25
        assert!(!Id::from("ch_0123456789abcdefghijklmnop").is_valid()); // lowercase
        assert!(!Id::from("_0123456789ABCDEFGHJKMNPQRSTVWXY").is_valid()); // no prefix
    }

    #[test]
    fn time_sortable_in_generation_order() {
        let a = new_id("x");
        let b = new_id("x");
        assert!(b >= a, "later id should not sort before earlier id");
    }
}
