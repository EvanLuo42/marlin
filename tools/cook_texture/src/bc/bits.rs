//! LSB-first bit streams over a 128-bit block (BC6H and BC7).

pub(crate) struct BitReader {
    bits: u128,
    pos: u32,
}

impl BitReader {
    pub(crate) fn new(block: &[u8; 16]) -> Self {
        Self {
            bits: u128::from_le_bytes(*block),
            pos: 0,
        }
    }

    pub(crate) fn read(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let v = (self.bits >> self.pos) as u32 & (u32::MAX >> (32 - count));
        self.pos += count;
        v
    }
}

#[derive(Default)]
pub(crate) struct BitWriter {
    bits: u128,
    pos: u32,
}

impl BitWriter {
    pub(crate) fn write(&mut self, value: u32, count: u32) {
        if count == 0 {
            return;
        }
        debug_assert!(
            count == 32 || value >> count == 0,
            "value wider than its field"
        );
        self.bits |= (value as u128) << self.pos;
        self.pos += count;
    }

    pub(crate) fn finish(self) -> [u8; 16] {
        debug_assert_eq!(self.pos, 128, "block size");
        self.bits.to_le_bytes()
    }
}
