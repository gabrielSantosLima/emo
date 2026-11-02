//! Cartridge headers, mapping modes and save RAM.
//!
//! A ROM image is not a flat blob: the SNES address space is carved into
//! windows by a mapping mode described in the cartridge header at `$FFC0`.
//! This module decodes that header and translates `bank:addr` accesses into
//! offsets inside the ROM or SRAM. See `docs/theory/` (section 09) for the
//! hardware background.

use std::fmt;

use crate::VideoStandard;

/// Address where the header sits in a LoROM image.
const LOROM_HEADER: usize = 0x7FC0;
/// Address where the header sits in a HiROM image.
const HIROM_HEADER: usize = 0xFFC0;
/// Address where the header sits in an ExHiROM image.
const EXHIROM_HEADER: usize = 0x40FFC0;

/// How the cartridge maps ROM into the SNES address space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mapping {
    /// 32 KB banks in the upper half of each bank.
    LoRom,
    /// Full 64 KB banks.
    HiRom,
    /// Extended HiROM, above 4 MB.
    ExHiRom,
}

impl Mapping {
    /// Header byte value expected for this mapping.
    const fn header_map_values(self) -> &'static [u8] {
        match self {
            Self::LoRom => &[0x20, 0x30],
            Self::HiRom => &[0x21, 0x31],
            Self::ExHiRom => &[0x25, 0x35],
        }
    }
}

impl fmt::Display for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LoRom => "LoROM",
            Self::HiRom => "HiROM",
            Self::ExHiRom => "ExHiROM",
        })
    }
}

/// Coprocessor declared by the cartridge type byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Coprocessor {
    /// No coprocessor.
    None,
    /// DSP-1/1B/2/3/4 math coprocessor.
    Dsp,
    /// SuperFX / GSU.
    SuperFx,
    /// SA-1.
    Sa1,
    /// S-DD1.
    Sdd1,
    /// CX4.
    Cx4,
    /// An undocumented type byte.
    Unknown(u8),
}

impl Coprocessor {
    /// Decode the cartridge type byte.
    #[must_use]
    pub const fn from_type_byte(value: u8) -> Self {
        match value {
            0x00..=0x02 => Self::None,
            0x03..=0x05 | 0x0F => Self::Dsp,
            0x13..=0x15 => Self::SuperFx,
            0x23 | 0x34 | 0x35 => Self::Sa1,
            0x33 | 0x43 => Self::Sdd1,
            0xF3..=0xF6 => Self::Cx4,
            other => Self::Unknown(other),
        }
    }

    /// True when this emulator can run the cartridge.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::None)
    }
}

impl fmt::Display for Coprocessor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Dsp => f.write_str("DSP"),
            Self::SuperFx => f.write_str("SuperFX"),
            Self::Sa1 => f.write_str("SA-1"),
            Self::Sdd1 => f.write_str("S-DD1"),
            Self::Cx4 => f.write_str("CX4"),
            Self::Unknown(value) => write!(f, "unknown ({value:#04x})"),
        }
    }
}

/// Decoded cartridge header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CartridgeInfo {
    /// Game title, trimmed of trailing spaces and NUL bytes.
    pub title: String,
    /// Mapping mode used by the image.
    pub mapping: Mapping,
    /// ROM size in bytes (the actual image length).
    pub rom_size: usize,
    /// Save RAM size in bytes, from the header.
    pub sram_size: usize,
    /// Region byte from the header.
    pub region: u8,
    /// Video standard implied by the region.
    pub video_standard: VideoStandard,
    /// Coprocessor, if any.
    pub coprocessor: Coprocessor,
    /// Mask ROM version.
    pub version: u8,
    /// Header checksum as stored in the image.
    pub checksum: u16,
    /// True when the stored checksum and its complement agree.
    pub checksum_valid: bool,
}

/// A loaded cartridge: ROM image plus battery-backed save RAM.
#[derive(Clone, Debug)]
pub struct Cartridge {
    rom: Vec<u8>,
    sram: Vec<u8>,
    info: CartridgeInfo,
}

/// Errors raised while loading a cartridge.
#[derive(Debug)]
pub enum CartridgeError {
    /// The image is too small to contain a header.
    Empty,
    /// The detected mapping cannot be used.
    UnsupportedMapping(Mapping),
    /// The cartridge declares a coprocessor this emulator does not implement.
    UnsupportedCoprocessor(Coprocessor),
}

impl fmt::Display for CartridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("ROM image is empty"),
            Self::UnsupportedMapping(mapping) => write!(f, "unsupported mapping: {mapping}"),
            Self::UnsupportedCoprocessor(chip) => {
                write!(f, "unsupported coprocessor: {chip}")
            }
        }
    }
}

impl std::error::Error for CartridgeError {}

impl Cartridge {
    /// Decode the header of `rom` and build a cartridge around it.
    pub fn from_rom(rom: Vec<u8>) -> Result<Self, CartridgeError> {
        if rom.is_empty() {
            return Err(CartridgeError::Empty);
        }

        let (mapping, header, score) = detect_mapping(&rom);
        let info = decode_header(&rom, mapping, header, score);
        let sram = vec![0; info.sram_size];

        Ok(Self { rom, sram, info })
    }

    /// Decoded header.
    #[must_use]
    pub const fn info(&self) -> &CartridgeInfo {
        &self.info
    }

    /// Raw ROM image.
    #[must_use]
    pub fn rom(&self) -> &[u8] {
        &self.rom
    }

    /// Current save RAM contents.
    #[must_use]
    pub fn sram(&self) -> &[u8] {
        &self.sram
    }

    /// Mutable save RAM contents.
    #[must_use]
    pub fn sram_mut(&mut self) -> &mut [u8] {
        &mut self.sram
    }

    /// Replace save RAM contents, truncating or zero-padding as needed.
    pub fn load_sram(&mut self, data: &[u8]) {
        self.sram.fill(0);
        let len = data.len().min(self.sram.len());
        self.sram[..len].copy_from_slice(&data[..len]);
    }

    /// ROM offset for a `bank:addr` access, or `None` when unmapped.
    #[must_use]
    pub fn rom_offset(&self, bank: u8, addr: u16) -> Option<usize> {
        let len = self.rom.len();
        if len == 0 {
            return None;
        }

        let offset = match self.info.mapping {
            Mapping::LoRom => {
                if bank == 0x7E || bank == 0x7F || addr < 0x8000 {
                    return None;
                }
                (usize::from(bank & 0x7F) << 15) | usize::from(addr & 0x7FFF)
            }
            Mapping::HiRom => {
                if bank >= 0xC0 || (matches!(bank & 0x7F, 0x00..=0x3F) && addr >= 0x8000) {
                    (usize::from(bank & 0x3F) << 16) | usize::from(addr)
                } else {
                    return None;
                }
            }
            Mapping::ExHiRom => {
                if bank >= 0xC0 {
                    (usize::from(bank & 0x3F) << 16) | usize::from(addr)
                } else if (0x40..=0x7D).contains(&bank)
                    || (matches!(bank & 0x7F, 0x00..=0x3F) && addr >= 0x8000)
                {
                    0x40_0000 | (usize::from(bank & 0x3F) << 16) | usize::from(addr)
                } else {
                    return None;
                }
            }
        };

        Some(if len.is_power_of_two() {
            offset & (len - 1)
        } else {
            offset % len
        })
    }

    /// SRAM offset for a `bank:addr` access, or `None` when unmapped.
    #[must_use]
    pub fn sram_offset(&self, bank: u8, addr: u16) -> Option<usize> {
        if self.sram.is_empty() {
            return None;
        }
        let len = self.sram.len();

        let offset = match self.info.mapping {
            Mapping::LoRom => {
                let page = bank & 0x7F;
                if !(0x70..=0x7D).contains(&page) || addr >= 0x8000 {
                    return None;
                }
                (usize::from(bank & 0x0F) << 15) | usize::from(addr)
            }
            Mapping::HiRom | Mapping::ExHiRom => {
                let page = bank & 0x7F;
                if !(0x20..=0x3F).contains(&page) || !(0x6000..0x8000).contains(&addr) {
                    return None;
                }
                (usize::from(bank & 0x1F) << 13) | usize::from(addr & 0x1FFF)
            }
        };

        Some(if len.is_power_of_two() {
            offset & (len - 1)
        } else {
            offset % len
        })
    }

    /// True when the cartridge declares a coprocessor this emulator lacks.
    #[must_use]
    pub fn needs_coprocessor(&self) -> bool {
        !self.info.coprocessor.is_supported()
    }
}

fn detect_mapping(rom: &[u8]) -> (Mapping, usize, u8) {
    let candidates = [
        (Mapping::LoRom, LOROM_HEADER),
        (Mapping::HiRom, HIROM_HEADER),
        (Mapping::ExHiRom, EXHIROM_HEADER),
    ];

    let mut best = (Mapping::LoRom, LOROM_HEADER, 0u8);
    for (mapping, base) in candidates {
        if base + 0x20 > rom.len() {
            continue;
        }
        let score = header_score(rom, base, mapping);
        if score > best.2 {
            best = (mapping, base, score);
        }
    }

    best
}

fn header_score(rom: &[u8], base: usize, mapping: Mapping) -> u8 {
    let header = &rom[base..base + 0x20];
    let mut score = 0;

    let complement = u16::from_le_bytes([header[0x1C], header[0x1D]]);
    let checksum = u16::from_le_bytes([header[0x1E], header[0x1F]]);
    if checksum ^ complement == 0xFFFF {
        score += 8;
    }

    if mapping.header_map_values().contains(&header[0x15]) {
        score += 4;
    }

    let printable = header[..0x15]
        .iter()
        .filter(|byte| byte.is_ascii_graphic() || **byte == b' ')
        .count();
    if printable >= 0x15 - 2 {
        score += 2;
    }

    if header[0x17] >= 0x05 && header[0x17] <= 0x0D {
        score += 1;
    }

    score
}

fn decode_header(rom: &[u8], mapping: Mapping, base: usize, score: u8) -> CartridgeInfo {
    let header = &rom[base..base + 0x20];

    let title = header[..0x15]
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .map(|byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                byte
            } else {
                b'?'
            }
        })
        .collect::<Vec<u8>>();
    let title = String::from_utf8_lossy(&title).trim_end().to_string();

    let region = header[0x19];
    let sram_size = if header[0x18] == 0 {
        0
    } else {
        1usize << (usize::from(header[0x18]) + 10)
    };
    let complement = u16::from_le_bytes([header[0x1C], header[0x1D]]);
    let checksum = u16::from_le_bytes([header[0x1E], header[0x1F]]);

    CartridgeInfo {
        title,
        mapping,
        rom_size: rom.len(),
        sram_size,
        region,
        video_standard: video_standard(region),
        coprocessor: Coprocessor::from_type_byte(header[0x16]),
        version: header[0x1B],
        checksum,
        checksum_valid: score & 0x08 != 0 && checksum ^ complement == 0xFFFF,
    }
}

/// Video standard implied by the region byte.
#[must_use]
pub const fn video_standard(region: u8) -> VideoStandard {
    match region {
        0x02..=0x06 | 0x09..=0x0B => VideoStandard::Pal,
        _ => VideoStandard::Ntsc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TITLE: &[u8] = b"EMO TEST ROM";

    fn synthetic(
        mapping: Mapping,
        rom_kb: usize,
        sram_exp: u8,
        region: u8,
        cart_type: u8,
    ) -> Vec<u8> {
        let size = rom_kb * 1024;
        let mut rom = vec![0u8; size];
        let base = match mapping {
            Mapping::LoRom => LOROM_HEADER,
            Mapping::HiRom => HIROM_HEADER,
            Mapping::ExHiRom => EXHIROM_HEADER,
        };
        rom[base..base + TITLE.len()].copy_from_slice(TITLE);
        rom[base + 0x15] = mapping.header_map_values()[0];
        rom[base + 0x16] = cart_type;
        rom[base + 0x17] = (rom_kb.trailing_zeros() as u8).saturating_sub(10);
        rom[base + 0x18] = sram_exp;
        rom[base + 0x19] = region;
        rom[base + 0x1B] = 1;

        let sum: u16 = rom
            .iter()
            .enumerate()
            .filter(|(index, _)| !(*index == base + 0x1E || *index == base + 0x1F))
            .fold(0u16, |acc, (_, byte)| acc.wrapping_add(u16::from(*byte)));
        rom[base + 0x1E..base + 0x20].copy_from_slice(&sum.to_le_bytes());
        rom[base + 0x1C..base + 0x1E].copy_from_slice(&(!sum).to_le_bytes());
        rom
    }

    #[test]
    fn detects_lorom_header() {
        let cart = Cartridge::from_rom(synthetic(Mapping::LoRom, 512, 1, 1, 0)).unwrap();
        let info = cart.info();

        assert_eq!(info.mapping, Mapping::LoRom);
        assert_eq!(info.title, "EMO TEST ROM");
        assert_eq!(info.rom_size, 512 * 1024);
        assert_eq!(info.sram_size, 2048);
        assert_eq!(info.video_standard, VideoStandard::Ntsc);
        assert!(info.checksum_valid);
        assert_eq!(info.coprocessor, Coprocessor::None);
    }

    #[test]
    fn detects_hirom_header() {
        let cart = Cartridge::from_rom(synthetic(Mapping::HiRom, 1024, 0, 2, 0)).unwrap();

        assert_eq!(cart.info().mapping, Mapping::HiRom);
        assert_eq!(cart.info().video_standard, VideoStandard::Pal);
        assert_eq!(cart.info().sram_size, 0);
    }

    #[test]
    fn lorom_offsets_follow_32k_banks() {
        let cart = Cartridge::from_rom(synthetic(Mapping::LoRom, 512, 0, 1, 0)).unwrap();

        assert_eq!(cart.rom_offset(0x00, 0x8000), Some(0x0000));
        assert_eq!(cart.rom_offset(0x00, 0xFFFF), Some(0x7FFF));
        assert_eq!(cart.rom_offset(0x01, 0x8000), Some(0x8000));
        assert_eq!(cart.rom_offset(0x00, 0x0000), None);
        assert_eq!(cart.rom_offset(0x7E, 0x8000), None);
    }

    #[test]
    fn hirom_offsets_cover_full_banks() {
        let cart = Cartridge::from_rom(synthetic(Mapping::HiRom, 1024, 0, 1, 0)).unwrap();

        assert_eq!(cart.rom_offset(0xC0, 0x0000), Some(0x0000));
        assert_eq!(cart.rom_offset(0xC0, 0xFFFF), Some(0xFFFF));
        assert_eq!(cart.rom_offset(0xC1, 0x0000), Some(0x1_0000));
        assert_eq!(cart.rom_offset(0x00, 0x8000), Some(0x8000));
        assert_eq!(cart.rom_offset(0x20, 0x0000), None);
    }

    #[test]
    fn sram_windows_depend_on_mapping() {
        let mut lorom = Cartridge::from_rom(synthetic(Mapping::LoRom, 256, 1, 1, 0)).unwrap();
        let lo = lorom.sram_offset(0x70, 0x0000).unwrap();
        lorom.sram[lo] = 0x5A;
        assert_eq!(&lorom.sram()[..1], &[0x5A]);
        assert_eq!(lorom.sram_offset(0x00, 0x6000), None);

        let hirom = Cartridge::from_rom(synthetic(Mapping::HiRom, 256, 1, 1, 0)).unwrap();
        assert!(hirom.sram_offset(0x20, 0x6000).is_some());
        assert!(hirom.sram_offset(0x20, 0x0000).is_none());
    }

    #[test]
    fn coprocessors_are_detected_and_refused() {
        let cart = Cartridge::from_rom(synthetic(Mapping::LoRom, 256, 0, 1, 0x13)).unwrap();

        assert_eq!(cart.info().coprocessor, Coprocessor::SuperFx);
        assert!(cart.needs_coprocessor());
    }

    #[test]
    fn pal_regions_map_to_the_pal_clock() {
        assert_eq!(video_standard(0x00), VideoStandard::Ntsc);
        assert_eq!(video_standard(0x01), VideoStandard::Ntsc);
        assert_eq!(video_standard(0x02), VideoStandard::Pal);
        assert_eq!(video_standard(0x06), VideoStandard::Pal);
    }
}
