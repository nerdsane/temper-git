//! SHA-1 — streaming implementation, no external deps.
//!
//! This is textbook SHA-1 (FIPS 180-1) adapted for streaming input.
//! It is used for a single purpose: computing git object hashes.
//!
//! **Why inline implementation.** `sha1` / `sha-1` / `openssl` would
//! all work, but each pulls in more code than the ~180 lines here,
//! and our WASM size budget is tight. SHA-1 is also cryptographically
//! retired and will not receive algorithmic updates — there is no
//! benefit to tracking a third-party crate's version stream.
//!
//! **Do not use this for anything other than git object hashing.** SHA-1
//! is broken for collision resistance. If you need a cryptographic hash
//! for a new feature, use SHA-256 and pull in a vetted crate.

/// Streaming SHA-1 hasher. Add bytes with [`Self::update`], finalize
/// with [`Self::finalize`] to get the 20-byte digest, or use
/// [`Self::hex`] for the 40-char lowercase hex.
#[derive(Clone)]
pub struct Sha1 {
    h: [u32; 5],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha1 {
    /// Fresh hasher initialized to SHA-1's standard constants.
    pub fn new() -> Self {
        Self {
            h: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            buf: [0; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    /// Feed bytes into the hasher.
    pub fn update(&mut self, input: &[u8]) {
        let mut input = input;
        self.total_len += input.len() as u64;
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(input.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&input[..take]);
            self.buf_len += take;
            input = &input[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.process_block(&block);
                self.buf_len = 0;
            }
        }
        while input.len() >= 64 {
            let block: [u8; 64] = input[..64].try_into().expect("64 bytes available");
            self.process_block(&block);
            input = &input[64..];
        }
        if !input.is_empty() {
            self.buf[..input.len()].copy_from_slice(input);
            self.buf_len = input.len();
        }
    }

    /// Consume the hasher and return the 20-byte SHA-1 digest.
    pub fn finalize(mut self) -> [u8; 20] {
        let bit_len = self.total_len.wrapping_mul(8);
        // Pad: one 0x80 byte, zeros, 8-byte big-endian bit count.
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;
        if self.buf_len > 56 {
            for b in self.buf[self.buf_len..].iter_mut() {
                *b = 0;
            }
            let block = self.buf;
            self.process_block(&block);
            self.buf_len = 0;
        }
        for b in self.buf[self.buf_len..56].iter_mut() {
            *b = 0;
        }
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.process_block(&block);

        let mut out = [0u8; 20];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// Consume the hasher and return the 40-char lowercase hex digest.
    pub fn hex(self) -> String {
        let d = self.finalize();
        sha1_digest_hex(&d)
    }

    fn process_block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) =
            (self.h[0], self.h[1], self.h[2], self.h[3], self.h[4]);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
    }
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience: SHA-1 of a whole byte slice, returned as 40-char hex.
pub fn sha1_hex(input: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(input);
    h.hex()
}

/// Render a 20-byte digest as 40-char lowercase hex.
pub fn sha1_digest_hex(d: &[u8; 20]) -> String {
    let mut out = String::with_capacity(40);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in d {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical SHA-1 test vectors from NIST / RFC 3174.
    #[test]
    fn empty() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn abc() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn fips_448_bits() {
        // "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        let input = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(sha1_hex(input), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    }

    #[test]
    fn one_million_a() {
        // 10^6 repetitions of 'a' — exercises multi-block + length handling.
        let mut h = Sha1::new();
        let chunk = [b'a'; 1024];
        for _ in 0..(1_000_000 / 1024) {
            h.update(&chunk);
        }
        h.update(&chunk[..1_000_000 % 1024]);
        assert_eq!(h.hex(), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn streaming_equivalent() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let one_shot = sha1_hex(data);
        let mut h = Sha1::new();
        for &byte in data {
            h.update(&[byte]);
        }
        assert_eq!(h.hex(), one_shot);
    }
}
