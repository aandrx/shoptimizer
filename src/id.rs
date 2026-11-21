//! Packed IDs (D6): an ID of at most 15 bytes as one `u128`.
//!
//! The bytes sit big-endian from bit 127 down, zero-padded, with the length in
//! the lowest byte. Integer order is then exactly byte-wise order: the first
//! differing byte decides, and when one ID is a prefix of the other the padding
//! (or, for an embedded `0x00`, the length byte) makes the shorter one smaller.

/// `None` when the ID is longer than 15 bytes (the caller falls back to V1).
#[inline]
pub fn pack(bytes: &[u8]) -> Option<u128> {
    if bytes.len() > 15 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf[..bytes.len()].copy_from_slice(bytes);
    buf[15] = bytes.len() as u8;
    Some(u128::from_be_bytes(buf))
}

/// The original bytes of a packed ID, written into `buf`.
#[inline]
pub fn unpack(value: u128, buf: &mut [u8; 16]) -> &[u8] {
    *buf = value.to_be_bytes();
    let len = buf[15] as usize;
    &buf[..len]
}

/// A well-mixed 64-bit hash of a packed ID.
#[inline]
pub fn mix(value: u128) -> u64 {
    let lo = value as u64;
    let hi = (value >> 64) as u64;
    let mut x = lo.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ hi.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    x ^= x >> 32;
    x = x.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    x ^= x >> 29;
    x
}

/// Bucket in `0..1 << bits` from the hash's top bits.
#[inline]
pub fn bucket(value: u128, bits: u32) -> usize {
    if bits == 0 {
        0
    } else {
        (mix(value) >> (64 - bits)) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn random_id(rng: &mut Rng) -> Vec<u8> {
        const ALPHABET: &[u8] = b"\x00 01aAuUzZ\xff";
        let len = (rng.next() % 16) as usize;
        (0..len)
            .map(|_| ALPHABET[(rng.next() % ALPHABET.len() as u64) as usize])
            .collect()
    }

    #[test]
    fn order_matches_bytewise_order() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        for _ in 0..200_000 {
            let a = random_id(&mut rng);
            let b = random_id(&mut rng);
            let (pa, pb) = (pack(&a).unwrap(), pack(&b).unwrap());
            assert_eq!(pa.cmp(&pb), a.cmp(&b), "{a:?} vs {b:?}");
            assert_eq!(pa == pb, a == b, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn named_examples() {
        let ids: [&[u8]; 5] = [b"U1", b"u1", b"u1 ", b"u10", b"u2"];
        for pair in ids.windows(2) {
            assert!(pack(pair[0]).unwrap() < pack(pair[1]).unwrap());
        }
        assert!(pack(b"a").unwrap() < pack(b"a\x00").unwrap());
    }

    #[test]
    fn round_trip_and_overflow() {
        let mut buf = [0u8; 16];
        for id in [&b""[..], b"u1", b"123456789012345", b"\x00\x00"] {
            assert_eq!(unpack(pack(id).unwrap(), &mut buf), id);
        }
        assert_eq!(pack(b"1234567890123456"), None);
    }

    #[test]
    fn buckets_are_balanced() {
        let mut counts = [0usize; 256];
        for i in 0..1_000_000u32 {
            let id = format!("e{:08}", i);
            counts[bucket(pack(id.as_bytes()).unwrap(), 8)] += 1;
        }
        let mean = 1_000_000 / 256;
        for c in counts {
            assert!(c > mean * 9 / 10 && c < mean * 11 / 10, "{c}");
        }
    }
}
