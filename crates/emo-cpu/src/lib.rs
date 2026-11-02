//! The 65C816 (S-CPU) instruction set core.
//!
//! This crate is deliberately isolated from the rest of the emulator: it knows
//! only a [`Bus`] trait, so it can be tested against flat memory and later
//! plugged into the real SNES address map by `emo-core`.

#![forbid(unsafe_code)]

pub mod bus;
mod cpu;

pub use bus::{Bus, FlatMemory};
pub use cpu::{Cpu, FLAG_C, FLAG_D, FLAG_I, FLAG_M, FLAG_N, FLAG_V, FLAG_X, FLAG_Z, Registers};
