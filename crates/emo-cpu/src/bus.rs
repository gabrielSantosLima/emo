//! The memory interface the CPU talks to.
//!
//! The CPU never touches memory directly. `emo-core` implements this trait
//! with the real SNES address map; tests and the vector runner use a flat
//! 16 MB image, because the SingleStepTests assume a single address space.

/// A 24-bit address space seen through the CPU's bus.
pub trait Bus {
    /// Read one byte at `bank:addr`.
    fn read(&mut self, bank: u8, addr: u16) -> u8;

    /// Write one byte at `bank:addr`.
    fn write(&mut self, bank: u8, addr: u16, value: u8);
}

/// A flat 16 MB memory image.
///
/// Useful for instruction-level tests and for the SingleStepTests runner,
/// which assumes all of the 24-bit address space is backed by RAM.
#[derive(Clone, Debug)]
pub struct FlatMemory {
    bytes: Box<[u8]>,
}

impl FlatMemory {
    /// Create a zeroed 16 MB address space.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes: vec![0; 1 << 24].into_boxed_slice(),
        }
    }

    fn index(bank: u8, addr: u16) -> usize {
        usize::from(bank) << 16 | usize::from(addr)
    }

    /// Read one byte through the flat mapping.
    #[must_use]
    pub fn read(&self, bank: u8, addr: u16) -> u8 {
        self.bytes[Self::index(bank, addr)]
    }

    /// Write one byte through the flat mapping.
    pub fn write(&mut self, bank: u8, addr: u16, value: u8) {
        self.bytes[Self::index(bank, addr)] = value;
    }

    /// Overwrite the whole image.
    pub fn load(&mut self, bytes: &[u8]) {
        let len = bytes.len().min(self.bytes.len());
        self.bytes[..len].copy_from_slice(&bytes[..len]);
    }
}

impl Default for FlatMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for FlatMemory {
    fn read(&mut self, bank: u8, addr: u16) -> u8 {
        Self::read(self, bank, addr)
    }

    fn write(&mut self, bank: u8, addr: u16, value: u8) {
        Self::write(self, bank, addr, value);
    }
}
