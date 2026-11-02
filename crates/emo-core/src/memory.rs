//! The CPU-visible address space.
//!
//! This module owns the decoding rules: which `bank:addr` window is WRAM,
//! which is cartridge ROM, which is save RAM, and what happens when nothing
//! answers. The PPU, APU and CPU register windows are wired in later
//! milestones; today they read as open bus.

use emo_cpu::Bus;

use crate::cartridge::Cartridge;

/// Work RAM size in bytes.
pub const WRAM_SIZE: usize = 128 * 1024;

/// Mirror of WRAM visible in the system banks.
const WRAM_MIRROR: usize = 8 * 1024;

/// The machine's memory map.
#[derive(Clone, Debug)]
pub struct Memory {
    wram: Box<[u8]>,
    cartridge: Cartridge,
    open_bus: u8,
}

impl Memory {
    /// Build the address space around a cartridge.
    #[must_use]
    pub fn new(cartridge: Cartridge) -> Self {
        Self {
            wram: vec![0; WRAM_SIZE].into_boxed_slice(),
            cartridge,
            open_bus: 0,
        }
    }

    /// Value most recently driven on the data bus.
    #[must_use]
    pub const fn open_bus(&self) -> u8 {
        self.open_bus
    }

    /// Cartridge currently inserted.
    #[must_use]
    pub const fn cartridge(&self) -> &Cartridge {
        &self.cartridge
    }

    /// Mutable cartridge, for save RAM access.
    pub fn cartridge_mut(&mut self) -> &mut Cartridge {
        &mut self.cartridge
    }

    fn read_mapped(&self, bank: u8, addr: u16) -> Option<u8> {
        if bank == 0x7E || bank == 0x7F {
            let offset = usize::from(bank - 0x7E) * 0x1_0000 + usize::from(addr);
            return Some(self.wram[offset]);
        }

        if addr < 0x2000 && !matches!(bank, 0x7E | 0x7F) {
            let in_system = bank < 0x40 || (0x80..=0xBF).contains(&bank);
            if in_system {
                return Some(self.wram[usize::from(addr) % WRAM_MIRROR]);
            }
        }

        if let Some(offset) = self.cartridge.sram_offset(bank, addr) {
            return Some(self.cartridge.sram()[offset]);
        }

        if let Some(offset) = self.cartridge.rom_offset(bank, addr) {
            return Some(self.cartridge.rom()[offset]);
        }

        None
    }

    fn write_mapped(&mut self, bank: u8, addr: u16, value: u8) -> bool {
        if bank == 0x7E || bank == 0x7F {
            let offset = usize::from(bank - 0x7E) * 0x1_0000 + usize::from(addr);
            self.wram[offset] = value;
            return true;
        }

        if addr < 0x2000 && !matches!(bank, 0x7E | 0x7F) {
            let in_system = bank < 0x40 || (0x80..=0xBF).contains(&bank);
            if in_system {
                let offset = usize::from(addr) % WRAM_MIRROR;
                self.wram[offset] = value;
                return true;
            }
        }

        if let Some(offset) = self.cartridge.sram_offset(bank, addr) {
            let sram = self.cartridge.sram_mut();
            sram[offset] = value;
            return true;
        }

        false
    }
}

impl Bus for Memory {
    fn read(&mut self, bank: u8, addr: u16) -> u8 {
        let value = self.read_mapped(bank, addr).unwrap_or(self.open_bus);
        self.open_bus = value;
        value
    }

    fn write(&mut self, bank: u8, addr: u16, value: u8) {
        self.write_mapped(bank, addr, value);
        self.open_bus = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cartridge::Mapping;

    fn cartridge(mapping: Mapping) -> Cartridge {
        // Minimal image with a valid header is enough for the map tests.
        let size = match mapping {
            Mapping::LoRom => 64 * 1024,
            Mapping::HiRom | Mapping::ExHiRom => 128 * 1024,
        };
        let mut rom = vec![0u8; size];
        let base = match mapping {
            Mapping::LoRom => 0x7FC0,
            Mapping::HiRom => 0xFFC0,
            Mapping::ExHiRom => 0x40FFC0,
        };
        if base + 0x20 <= rom.len() {
            rom[base..base + 4].copy_from_slice(b"TEST");
            rom[base + 0x15] = mapping_values(mapping);
            rom[base + 0x17] = 6;
            rom[base + 0x18] = 1;
            rom[base + 0x19] = 1;
        }
        Cartridge::from_rom(rom).expect("valid synthetic cartridge")
    }

    fn mapping_values(mapping: Mapping) -> u8 {
        match mapping {
            Mapping::LoRom => 0x20,
            Mapping::HiRom => 0x21,
            Mapping::ExHiRom => 0x25,
        }
    }

    #[test]
    fn wram_is_mirrored_in_the_system_banks() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom));

        memory.write(0x00, 0x0010, 0x42);
        assert_eq!(memory.read(0x01, 0x0010), 0x42);
        assert_eq!(memory.read(0x80, 0x0010), 0x42);
        assert_eq!(memory.read(0x7E, 0x0010), 0x42);

        memory.write(0x7F, 0xFFFF, 0x99);
        assert_eq!(memory.read(0x7F, 0xFFFF), 0x99);
    }

    #[test]
    fn rom_window_returns_cartridge_bytes() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom));

        assert_eq!(memory.read(0x00, 0xFFC0), b'T');
        assert_eq!(memory.read(0x00, 0xFFC1), b'E');
        assert_eq!(memory.read(0x01, 0xFFC0), 0x00);
    }

    #[test]
    fn sram_window_is_writable_and_survives_reads() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom));

        memory.write(0x70, 0x0000, 0x5A);
        assert_eq!(memory.read(0x70, 0x0000), 0x5A);
        assert_eq!(memory.cartridge().sram()[0], 0x5A);

        memory.cartridge_mut().load_sram(&[0x11, 0x22]);
        assert_eq!(memory.cartridge().sram()[0], 0x11);
        assert_eq!(memory.cartridge().sram()[1], 0x22);
    }

    #[test]
    fn unmapped_reads_return_the_last_bus_value() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom));

        memory.write(0x7E, 0x1234, 0xAB);
        assert_eq!(memory.read(0x21, 0x2100), 0xAB);
        assert_eq!(memory.open_bus(), 0xAB);
    }

    #[test]
    fn rom_writes_are_ignored_but_drive_the_bus() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom));
        let before = memory.cartridge().rom()[0];

        memory.write(0x00, 0x8000, 0x7F);

        assert_eq!(memory.cartridge().rom()[0], before);
        assert_eq!(memory.open_bus(), 0x7F);
    }
}
