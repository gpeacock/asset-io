//! Table-driven CRC-32 (IEEE 802.3, as used by PNG).

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static TABLE: [u32; 256] = make_table();

/// Incremental CRC-32.
#[derive(Debug, Clone)]
pub struct Crc32(u32);

impl Crc32 {
    /// Start a new checksum.
    pub fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    /// Feed bytes.
    pub fn update(&mut self, data: &[u8]) {
        let mut c = self.0;
        for &b in data {
            c = TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    /// Finish and return the checksum.
    pub fn finalize(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector() {
        let mut c = Crc32::new();
        c.update(b"123456789");
        assert_eq!(c.finalize(), 0xCBF4_3926);
    }

    #[test]
    fn incremental_matches_one_shot() {
        let mut a = Crc32::new();
        a.update(b"hello ");
        a.update(b"world");
        let mut b = Crc32::new();
        b.update(b"hello world");
        assert_eq!(a.finalize(), b.finalize());
    }
}
