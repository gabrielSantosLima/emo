//! Machine core for `emo`.
//!
//! This crate owns what the individual components must not: the master-cycle
//! clock, the address bus and the wiring between CPU, PPU and APU. Components
//! stay independent; the machine is what connects them.

#![forbid(unsafe_code)]

pub mod timing;

pub use timing::{MASTER_CLOCK_NTSC, MASTER_CLOCK_PAL, VideoStandard};
