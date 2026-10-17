//! CRC-32 (IEEE 802.3: reflected, polynomial 0xEDB88320, initial and final xor 0xFFFFFFFF), the
//! checksum of zlib, gzip, PNG and zip. Slicing-by-8 tables built at compile time; `const fn`.
//!
//! An error-detecting checksum, not a hash: use it where a format specifies it.

/// The reflected polynomial.
const POLY: u32 = 0xedb8_8320;

/// `TABLES[0]` is the classic byte-at-a-time table; `TABLES[k][b]` is the CRC of byte `b`
/// followed by `k` zero bytes, so eight bytes fold in with eight lookups.
const TABLES: [[u32; 256]; 8] = {
    let mut t = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        t[0][i] = crc;
        i += 1;
    }
    let mut k = 1;
    while k < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[k - 1][i];
            t[k][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            i += 1;
        }
        k += 1;
    }
    t
};

/// CRC-32 of `bytes`: `crc32(b"123456789") == 0xcbf43926`.
#[inline]
pub const fn crc32(bytes: &[u8]) -> u32 {
    crc32_extend(0, bytes)
}

/// Continues a CRC-32: `crc32_extend(crc32(a), b) == crc32(a ++ b)` (zlib's `crc32(crc, ...)`).
#[inline]
pub const fn crc32_extend(crc: u32, bytes: &[u8]) -> u32 {
    !update(!crc, bytes)
}

/// The register update, without the initial and final inversion.
const fn update(mut crc: u32, bytes: &[u8]) -> u32 {
    let t = &TABLES;
    let len = bytes.len();
    let mut i = 0;
    while i + 8 <= len {
        let lo = crc ^ u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let hi = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]);
        crc = t[7][(lo & 0xff) as usize]
            ^ t[6][((lo >> 8) & 0xff) as usize]
            ^ t[5][((lo >> 16) & 0xff) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][(hi & 0xff) as usize]
            ^ t[2][((hi >> 8) & 0xff) as usize]
            ^ t[1][((hi >> 16) & 0xff) as usize]
            ^ t[0][(hi >> 24) as usize];
        i += 8;
    }
    while i < len {
        crc = (crc >> 8) ^ t[0][((crc ^ bytes[i] as u32) & 0xff) as usize];
        i += 1;
    }
    crc
}

/// Streaming CRC-32: `update` any number of times, then [`finish`](Self::finish).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Crc32 {
    /// The CRC of the input so far (finished form, so `finish` is a plain read).
    crc: u32,
}

impl Crc32 {
    #[inline]
    pub const fn new() -> Self {
        Self { crc: 0 }
    }

    /// Continues from the CRC of earlier input.
    #[inline]
    pub const fn with_initial(crc: u32) -> Self {
        Self { crc }
    }

    #[inline]
    pub const fn update(&mut self, bytes: &[u8]) {
        self.crc = crc32_extend(self.crc, bytes);
    }

    /// The CRC of everything so far. More input may follow.
    #[inline]
    pub const fn finish(&self) -> u32 {
        self.crc
    }
}
