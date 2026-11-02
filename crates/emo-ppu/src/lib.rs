//! The S-PPU video chip.
//!
//! The PPU is a beam-driven renderer: registers are read by the hardware as it
//! scans, so a change written between two scanlines affects only the lines that
//! follow. This crate renders one scanline at a time, which is enough for the
//! large majority of effects; per-dot accuracy for raster tricks comes later.

#![forbid(unsafe_code)]

mod ppu;

pub use ppu::{Ppu, SCREEN_WIDTH, Timing};
