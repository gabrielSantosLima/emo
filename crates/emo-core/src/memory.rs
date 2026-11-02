//! The CPU-visible address space.
//!
//! This module owns the decoding rules: which `bank:addr` window is WRAM,
//! which is cartridge ROM, which is save RAM, and what happens when nothing
//! answers. The PPU, APU and CPU register windows are wired in later
//! milestones; today they read as open bus.

use emo_apu::Apu;
use emo_cpu::Bus;
use emo_ppu::{Ppu, Timing};

use crate::VideoStandard;
use crate::cartridge::Cartridge;
use crate::dma::{DmaChannel, b_address};

/// Work RAM size in bytes.
pub const WRAM_SIZE: usize = 128 * 1024;

/// Mirror of WRAM visible in the system banks.
const WRAM_MIRROR: usize = 8 * 1024;

/// The machine's memory map.
#[derive(Clone, Debug)]
pub struct Memory {
    wram: Box<[u8]>,
    cartridge: Cartridge,
    ppu: Ppu,
    apu: Apu,
    channels: [DmaChannel; 8],
    nmitimen: u8,
    nmi_flag: bool,
    hdma_enable: u8,
    pending_cycles: u64,
    open_bus: u8,
    buttons: [u16; 4],
    joy_latch: [u16; 4],
    manual_shift: [u16; 2],
    manual_index: [u8; 2],
    strobe: bool,
}

impl Memory {
    /// Build the address space around a cartridge.
    #[must_use]
    pub fn new(cartridge: Cartridge, standard: VideoStandard) -> Self {
        let timing = match standard {
            VideoStandard::Ntsc => Timing::ntsc(),
            VideoStandard::Pal => Timing::pal(),
        };

        Self {
            wram: vec![0; WRAM_SIZE].into_boxed_slice(),
            cartridge,
            ppu: Ppu::new(timing),
            apu: Apu::new(),
            channels: std::array::from_fn(|_| DmaChannel::default()),
            nmitimen: 0,
            nmi_flag: false,
            hdma_enable: 0,
            pending_cycles: 0,
            open_bus: 0,
            buttons: [0; 4],
            joy_latch: [0; 4],
            manual_shift: [0; 2],
            manual_index: [0; 2],
            strobe: false,
        }
    }

    /// Video chip.
    #[must_use]
    pub const fn ppu(&self) -> &Ppu {
        &self.ppu
    }

    /// Mutable video chip.
    pub fn ppu_mut(&mut self) -> &mut Ppu {
        &mut self.ppu
    }

    /// Audio subsystem.
    #[must_use]
    pub const fn apu(&self) -> &Apu {
        &self.apu
    }

    /// Mutable audio subsystem.
    pub fn apu_mut(&mut self) -> &mut Apu {
        &mut self.apu
    }

    /// True when `$4200` bit 7 enables NMI.
    #[must_use]
    pub const fn nmi_enabled(&self) -> bool {
        self.nmitimen & 0x80 != 0
    }

    /// Set the pressed buttons for a controller port (active high).
    ///
    /// The 16-bit word follows the auto-joypad layout:
    /// `byetUDLR axlr0000`, with bits 3-0 reserved for the controller ID.
    pub fn set_buttons(&mut self, port: usize, buttons: u16) {
        self.buttons[port & 3] = buttons & 0xfff0;
    }

    /// Latch the controller state, as auto-joypad does at the start of vblank.
    pub fn latch_joypad(&mut self) {
        if self.nmitimen & 0x01 == 0 {
            return;
        }
        self.joy_latch = self.buttons;
    }

    /// Record that the PPU raised NMI.
    pub fn note_nmi(&mut self) {
        self.nmi_flag = true;
    }

    fn read_manual_joypad(&mut self, port: usize) -> u8 {
        if self.strobe {
            // While the latch is held, the shift register keeps reloading and
            // every read returns the first bit (B, active low).
            return u8::from(self.manual_shift[port] & 0x8000 == 0);
        }
        let index = self.manual_index[port];
        let bit = if index < 12 {
            // The serial transfer sends bit 15 (B) first and is active low.
            let pressed = (self.manual_shift[port] >> (15 - index)) & 1;
            u8::from(pressed == 0)
        } else if index < 16 {
            // Signature: a standard controller reports 0000.
            0
        } else {
            1
        };
        self.manual_index[port] = (index + 1).min(31);
        bit
    }

    /// Take the latched NMI flag (`$4210` bit 7).
    pub fn take_nmi_flag(&mut self) -> bool {
        let flag = self.nmi_flag;
        self.nmi_flag = false;
        flag
    }

    /// Master cycles consumed by DMA/HDMA since the last call.
    pub fn take_pending_cycles(&mut self) -> u64 {
        let cycles = self.pending_cycles;
        self.pending_cycles = 0;
        cycles
    }

    /// Start a general-purpose DMA transfer for the selected channels.
    pub fn start_dma(&mut self, mask: u8) {
        for index in 0..8 {
            if mask & (1 << index) == 0 {
                continue;
            }

            let mut channel = self.channels[index].clone();
            let mut count = u32::from(channel.das);
            if count == 0 {
                count = 0x1_0000;
            }

            let mut address = (u32::from(channel.a1b) << 16) | u32::from(channel.a1t);
            let b = b_address(&channel);
            let offsets = channel.b_offsets();
            let step = channel.a_step();

            for index in 0..count {
                let offset = offsets[index as usize % offsets.len()];
                let b_addr = b.wrapping_add(u16::from(offset));

                if channel.reverse() {
                    let value = self.b_read(b_addr);
                    self.a_write(address, value);
                } else {
                    let value = self.a_read(address);
                    self.b_write(b_addr, value);
                }

                address = address.wrapping_add_signed(step);
            }

            channel.a1t = address as u16;
            channel.a1b = (address >> 16) as u8;
            channel.das = 0;
            self.channels[index] = channel;
            self.pending_cycles += 8 * u64::from(count) + 8;
        }
    }

    /// Enable HDMA channels and reset their table cursors.
    pub fn write_hdma_enable(&mut self, value: u8) {
        self.hdma_enable = value;
        for index in 0..8 {
            if value & (1 << index) != 0 {
                self.channels[index].reset_hdma();
            }
        }
    }

    /// Run one HDMA line for every enabled channel.
    pub fn run_hdma_line(&mut self, _line: u16) {
        let mut cycles = 0u64;

        for index in 0..8 {
            if self.hdma_enable & (1 << index) == 0 {
                continue;
            }

            let mut channel = self.channels[index].clone();
            if channel.finished {
                continue;
            }

            if channel.remaining == 0 {
                let address =
                    (u32::from(channel.table_bank) << 16) | u32::from(channel.table_index);
                let header = self.a_read(address);
                channel.table_index = channel.table_index.wrapping_add(1);
                cycles += 8;

                if header == 0 {
                    channel.finished = true;
                    self.channels[index] = channel;
                    continue;
                }

                channel.remaining = header & 0x7F;
                channel.indirect = header & 0x80 != 0;
                if channel.remaining == 0 {
                    channel.remaining = 128;
                }

                if channel.indirect {
                    let lo = self.a_read(
                        (u32::from(channel.table_bank) << 16) | u32::from(channel.table_index),
                    );
                    let hi = self.a_read(
                        (u32::from(channel.table_bank) << 16)
                            | u32::from(channel.table_index.wrapping_add(1)),
                    );
                    let bank = self.a_read(
                        (u32::from(channel.table_bank) << 16)
                            | u32::from(channel.table_index.wrapping_add(2)),
                    );
                    channel.pointer = u16::from(lo) | (u16::from(hi) << 8);
                    channel.pointer_bank = bank;
                    channel.table_index = channel.table_index.wrapping_add(3);
                    cycles += 24;
                } else {
                    channel.pointer = channel.table_index;
                    channel.pointer_bank = channel.table_bank;
                }
            }

            let b = b_address(&channel);
            for offset in channel.b_offsets() {
                let b_addr = b.wrapping_add(u16::from(*offset));
                let source = (u32::from(channel.pointer_bank) << 16) | u32::from(channel.pointer);
                let value = self.a_read(source);
                self.b_write(b_addr, value);
                channel.pointer = channel.pointer.wrapping_add(1);
                cycles += 8;
            }

            if !channel.indirect {
                channel.table_index = channel.pointer;
            }
            channel.remaining -= 1;
            self.channels[index] = channel;
        }

        self.pending_cycles += cycles;
    }

    fn a_read(&mut self, address: u32) -> u8 {
        self.read_mapped((address >> 16) as u8, address as u16)
            .unwrap_or(self.open_bus)
    }

    fn a_write(&mut self, address: u32, value: u8) {
        self.write_mapped((address >> 16) as u8, address as u16, value);
    }

    fn b_write(&mut self, address: u16, value: u8) {
        if (0x2100..=0x213F).contains(&address) {
            self.ppu.write_register(address, value);
        } else if (0x2140..=0x2143).contains(&address) {
            self.apu.cpu_write(usize::from(address - 0x2140), value);
        }
    }

    fn b_read(&mut self, address: u16) -> u8 {
        if (0x2100..=0x213F).contains(&address) {
            self.ppu.read_register(address)
        } else {
            self.open_bus
        }
    }

    /// Value most recently driven on the data bus.
    #[must_use]
    pub const fn open_bus(&self) -> u8 {
        self.open_bus
    }

    /// Work RAM contents.
    #[must_use]
    pub fn wram(&self) -> &[u8] {
        &self.wram
    }

    /// Mutable work RAM, for save states and debugging.
    pub fn wram_mut(&mut self) -> &mut [u8] {
        &mut self.wram
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

    /// Read without touching the open-bus latch, or `None` when unmapped.
    #[must_use]
    pub fn peek(&mut self, bank: u8, addr: u16) -> Option<u8> {
        self.read_mapped(bank, addr)
    }

    fn read_mapped(&mut self, bank: u8, addr: u16) -> Option<u8> {
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

        if (0x2100..=0x213F).contains(&addr) && matches!(bank, 0x00..=0x3F | 0x80..=0xBF) {
            return Some(self.ppu.read_register(addr));
        }

        if (0x2140..=0x2143).contains(&addr) && matches!(bank, 0x00..=0x3F | 0x80..=0xBF) {
            return Some(self.apu.cpu_read(usize::from(addr - 0x2140)));
        }

        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) {
            match addr {
                0x4210 => {
                    let flag = self.take_nmi_flag();
                    return Some(u8::from(flag) << 7 | 0x02);
                }
                0x4211 => return Some(0),
                0x4212 => {
                    let vblank = self.ppu.in_vblank();
                    // Bit 0 is the auto-joypad busy flag. The latch completes
                    // as soon as vblank starts, so it is never busy here.
                    return Some(u8::from(vblank) << 7);
                }
                0x4016 => return Some(self.read_manual_joypad(0)),
                0x4017 => return Some(self.read_manual_joypad(1)),
                0x4213 => return Some(self.open_bus),
                0x4214..=0x4217 => return Some(0),
                0x4218..=0x421F => {
                    let index = usize::from(addr - 0x4218);
                    let port = index / 2;
                    let value = self.joy_latch[port];
                    // Auto-joypad registers are active high; bits 3-0 of the
                    // low byte are the controller ID (0000 for a standard pad).
                    return Some(if index % 2 == 0 {
                        value as u8 & 0xf0
                    } else {
                        (value >> 8) as u8
                    });
                }
                _ => {}
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

        if (0x2100..=0x213F).contains(&addr) && matches!(bank, 0x00..=0x3F | 0x80..=0xBF) {
            self.ppu.write_register(addr, value);
            return true;
        }

        if (0x2140..=0x2143).contains(&addr) && matches!(bank, 0x00..=0x3F | 0x80..=0xBF) {
            self.apu.cpu_write(usize::from(addr - 0x2140), value);
            return true;
        }

        if matches!(bank, 0x00..=0x3F | 0x80..=0xBF) {
            match addr {
                0x4200 => {
                    self.nmitimen = value;
                    return true;
                }
                0x4016 => {
                    self.strobe = value & 0x01 != 0;
                    if self.strobe {
                        self.manual_shift = [self.buttons[0], self.buttons[1]];
                        self.manual_index = [0; 2];
                    }
                    return true;
                }
                0x420B => {
                    self.start_dma(value);
                    return true;
                }
                0x420C => {
                    self.write_hdma_enable(value);
                    return true;
                }
                0x420D => return true,
                0x4300..=0x437F => {
                    let index = usize::from((addr - 0x4300) / 0x10);
                    let register = (addr - 0x4300) % 0x10;
                    let channel = &mut self.channels[index];
                    match register {
                        0x0 => channel.dmap = value,
                        0x1 => channel.bbad = value,
                        0x2 => channel.a1t = (channel.a1t & 0xFF00) | u16::from(value),
                        0x3 => channel.a1t = (channel.a1t & 0x00FF) | (u16::from(value) << 8),
                        0x4 => channel.a1b = value,
                        0x5 => channel.das = (channel.das & 0xFF00) | u16::from(value),
                        0x6 => channel.das = (channel.das & 0x00FF) | (u16::from(value) << 8),
                        0x7 => channel.dasb = value,
                        0x8 => channel.a2a = (channel.a2a & 0xFF00) | u16::from(value),
                        0x9 => channel.a2a = (channel.a2a & 0x00FF) | (u16::from(value) << 8),
                        0xA => channel.a2b = value,
                        _ => {}
                    }
                    return true;
                }
                _ => {}
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
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);

        memory.write(0x00, 0x0010, 0x42);
        assert_eq!(memory.read(0x01, 0x0010), 0x42);
        assert_eq!(memory.read(0x80, 0x0010), 0x42);
        assert_eq!(memory.read(0x7E, 0x0010), 0x42);

        memory.write(0x7F, 0xFFFF, 0x99);
        assert_eq!(memory.read(0x7F, 0xFFFF), 0x99);
    }

    #[test]
    fn rom_window_returns_cartridge_bytes() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);

        assert_eq!(memory.read(0x00, 0xFFC0), b'T');
        assert_eq!(memory.read(0x00, 0xFFC1), b'E');
        assert_eq!(memory.read(0x01, 0xFFC0), 0x00);
    }

    #[test]
    fn sram_window_is_writable_and_survives_reads() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);

        memory.write(0x70, 0x0000, 0x5A);
        assert_eq!(memory.read(0x70, 0x0000), 0x5A);
        assert_eq!(memory.cartridge().sram()[0], 0x5A);

        memory.cartridge_mut().load_sram(&[0x11, 0x22]);
        assert_eq!(memory.cartridge().sram()[0], 0x11);
        assert_eq!(memory.cartridge().sram()[1], 0x22);
    }

    #[test]
    fn auto_joypad_reports_active_low_buttons() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);
        memory.write(0x00, 0x4200, 0x81);
        memory.set_buttons(0, 0x1000); // Start

        memory.latch_joypad();

        assert_eq!(memory.read(0x00, 0x4218), 0x00);
        assert_eq!(memory.read(0x00, 0x4219), 0x10);
    }

    #[test]
    fn manual_joypad_shift_uses_serial_order() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);
        memory.set_buttons(0, 0x8000); // B

        memory.write(0x00, 0x4016, 1);
        memory.write(0x00, 0x4016, 0);

        let mut word = 0u16;
        for index in 0..16 {
            word |= u16::from(memory.read(0x00, 0x4016)) << (15 - index);
        }

        assert_eq!(word, 0x7ff0);
        assert_eq!(memory.read(0x00, 0x4016), 1);
    }

    #[test]
    fn unmapped_reads_return_the_last_bus_value() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);

        memory.write(0x7E, 0x1234, 0xAB);
        assert_eq!(memory.read(0x21, 0x2200), 0xAB);
        assert_eq!(memory.open_bus(), 0xAB);
    }

    #[test]
    fn dma_copies_wram_to_vram() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);

        memory.write(0x00, 0x2115, 0x80);
        memory.write(0x00, 0x2116, 0x00);
        memory.write(0x00, 0x2117, 0x00);

        memory.write(0x7E, 0x1000, 0x12);
        memory.write(0x7E, 0x1001, 0x34);

        memory.write(0x00, 0x4300, 0x01);
        memory.write(0x00, 0x4301, 0x18);
        memory.write(0x00, 0x4302, 0x00);
        memory.write(0x00, 0x4303, 0x10);
        memory.write(0x00, 0x4304, 0x7E);
        memory.write(0x00, 0x4305, 0x02);
        memory.write(0x00, 0x4306, 0x00);

        memory.write(0x00, 0x420B, 0x01);

        assert_eq!(memory.ppu().vram()[0], 0x12);
        assert_eq!(memory.ppu().vram()[1], 0x34);
        assert!(memory.take_pending_cycles() > 0);
    }

    #[test]
    fn hdma_writes_one_cgram_color_per_line() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);

        memory.write(0x00, 0x2121, 0x00);

        let table = [0x02, 0x1F, 0x00, 0xE0, 0x03, 0x00];
        for (index, byte) in table.iter().enumerate() {
            memory.write(0x7E, 0x0200 + index as u16, *byte);
        }

        memory.write(0x00, 0x4300, 0x02);
        memory.write(0x00, 0x4301, 0x22);
        memory.write(0x00, 0x4308, 0x00);
        memory.write(0x00, 0x4309, 0x02);
        memory.write(0x00, 0x430A, 0x7E);

        memory.write(0x00, 0x420C, 0x01);

        memory.run_hdma_line(0);
        memory.run_hdma_line(1);
        memory.run_hdma_line(2);

        assert_eq!(memory.ppu().cgram()[0], 0x001F);
        assert_eq!(memory.ppu().cgram()[1], 0x03E0);
    }

    #[test]
    fn nmi_flag_follows_the_enable_bit() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);

        assert!(!memory.nmi_enabled());
        memory.write(0x00, 0x4200, 0x80);
        assert!(memory.nmi_enabled());

        memory.note_nmi();
        assert!(memory.take_nmi_flag());
        assert!(!memory.take_nmi_flag());
    }

    #[test]
    fn rom_writes_are_ignored_but_drive_the_bus() {
        let mut memory = Memory::new(cartridge(Mapping::LoRom), crate::VideoStandard::Ntsc);
        let before = memory.cartridge().rom()[0];

        memory.write(0x00, 0x8000, 0x7F);

        assert_eq!(memory.cartridge().rom()[0], before);
        assert_eq!(memory.open_bus(), 0x7F);
    }
}
