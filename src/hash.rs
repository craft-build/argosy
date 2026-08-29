//! Content hashing shared by the index and packaging. **Not a security
//! boundary**: the digests are content addresses for staleness/change
//! detection and distribution integrity, never authentication. Backed by
//! the `sha2` crate (packaging already depended on it); the index's former
//! hand-rolled implementation was removed in its favor.

use sha2::{Digest, Sha256};

/// The lowercase hex SHA-256 digest of `data`.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex(&hasher.finalize())
}

/// Lowercase hex of raw digest bytes (for callers that stream updates
/// into the hasher themselves, e.g. `bundle_content_hash`).
pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_the_fips_180_4_vectors() {
        let cases: [(&[u8], &str); 4] = [
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"The quick brown fox jumps over the lazy dog",
                "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592",
            ),
            // 56 bytes: the padding spills into a second block — the
            // boundary case the hand-rolled version was pinned against.
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(sha256_hex(input), expected, "input {input:?}");
        }
    }
}
