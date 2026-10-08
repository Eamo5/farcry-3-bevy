//! Bit reader for Bink packets. Bink packs its fields least significant bit first, so a
//! field's first bit is the lowest unread bit of the current byte.

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Read position in bits.
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// The next 57 or more unread bits, lowest first. Past the end of the data the bits read as zero,
    /// so a damaged packet decodes to garbage instead of panicking.
    #[inline(always)]
    fn window(&self) -> u64 {
        let byte = self.pos >> 3;
        let word = match self.data.get(byte..byte + 8) {
            Some(bytes) => u64::from_le_bytes(bytes.try_into().unwrap()),
            None => self.window_tail(byte),
        };
        word >> (self.pos & 7)
    }

    #[cold]
    fn window_tail(&self, byte: usize) -> u64 {
        let mut bytes = [0u8; 8];
        if byte < self.data.len() {
            let n = (self.data.len() - byte).min(8);
            bytes[..n].copy_from_slice(&self.data[byte..byte + n]);
        }
        u64::from_le_bytes(bytes)
    }

    /// The next `n` bits (at most 32) without consuming them.
    #[inline(always)]
    pub fn peek(&self, n: u32) -> u32 {
        (self.window() & ((1u64 << n) - 1)) as u32
    }

    #[inline(always)]
    pub fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    /// Reads an `n`-bit unsigned field (at most 32 bits; zero bits reads 0).
    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    #[inline(always)]
    pub fn bit(&mut self) -> bool {
        self.read(1) != 0
    }

    /// Bits still unread; negative once a damaged packet has been over-read.
    pub fn left(&self) -> isize {
        (self.data.len() * 8) as isize - self.pos as isize
    }

    /// Skips to the next multiple of 32 bits, where each plane and each audio block starts.
    pub fn align32(&mut self) {
        self.pos = (self.pos + 31) & !31;
    }
}
