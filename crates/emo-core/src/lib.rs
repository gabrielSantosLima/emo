//! Machine core for `emo`.
//!
//! This crate owns what the individual components must not: the master-cycle
//! clock, the address bus and the wiring between CPU, PPU and APU. Components
//! stay independent; the machine is what connects them.

#![forbid(unsafe_code)]

pub mod cartridge;
pub mod memory;
pub mod timing;

pub use cartridge::{Cartridge, CartridgeError, CartridgeInfo, Coprocessor, Mapping};
pub use memory::Memory;
pub use timing::{MASTER_CLOCK_NTSC, MASTER_CLOCK_PAL, VideoStandard};
