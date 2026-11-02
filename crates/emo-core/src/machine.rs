//! The machine: CPU, memory and the master-clock frame scheduler.
//!
//! The machine owns emulated time. Everything runs as fast as the host allows,
//! but the guest only ever sees master cycles, which is what makes emulation
//! deterministic and save states reproducible.

use std::collections::VecDeque;
use std::fmt;

use emo_cpu::Cpu;
use serde::{Deserialize, Serialize};

use crate::cartridge::{Cartridge, Coprocessor};
use crate::memory::Memory;
use crate::timing::VideoStandard;

/// Master cycles in one CPU machine cycle.
///
/// The S-CPU runs at `master / 6`. Slow regions add wait states on top; those
/// are a bus concern and land with the PPU/APU register windows.
pub const MASTER_CYCLES_PER_CPU_CYCLE: u64 = 6;

/// Version written into serialized save states.
const STATE_VERSION: u32 = 1;

/// One executed S-CPU instruction, for debugging.
#[derive(Clone, Copy, Debug)]
pub struct CpuTrace {
    /// Program bank.
    pub pbr: u8,
    /// Program counter before the instruction.
    pub pc: u16,
    /// Opcode byte.
    pub opcode: u8,
    /// Accumulator after the instruction.
    pub a: u16,
    /// X after the instruction.
    pub x: u16,
    /// Y after the instruction.
    pub y: u16,
    /// Stack pointer after the instruction.
    pub sp: u16,
    /// Status byte after the instruction.
    pub p: u8,
}

/// Result of running one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameResult {
    /// Frame index, starting at zero after reset.
    pub frame: u64,
    /// Master clock at the end of the frame.
    pub clock: u64,
    /// Fingerprint of the machine state at the end of the frame.
    pub hash: u64,
}

/// Errors raised by machine operations.
#[derive(Debug)]
pub enum MachineError {
    /// The cartridge needs a coprocessor that is not implemented.
    Coprocessor(Coprocessor),
}

impl fmt::Display for MachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coprocessor(chip) => {
                write!(f, "cartridge requires an unsupported coprocessor: {chip}")
            }
        }
    }
}

impl std::error::Error for MachineError {}

/// Errors raised while serializing or restoring a save state.
#[derive(Debug)]
pub enum StateError {
    /// The blob was produced by a different save-state version.
    Version {
        /// Version this build writes.
        expected: u32,
        /// Version found in the blob.
        found: u32,
    },
    /// The blob could not be encoded.
    Encode(String),
    /// The blob could not be decoded.
    Decode(String),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version { expected, found } => {
                write!(
                    f,
                    "save state version {found} is not supported (expected {expected})"
                )
            }
            Self::Encode(message) => write!(f, "failed to encode save state: {message}"),
            Self::Decode(message) => write!(f, "failed to decode save state: {message}"),
        }
    }
}

impl std::error::Error for StateError {}

#[derive(Serialize, Deserialize)]
struct StateBlob {
    version: u32,
    clock: u64,
    frame: u64,
    cpu: Cpu,
    ppu: emo_ppu::Ppu,
    apu: emo_apu::Apu,
    wram: Vec<u8>,
    sram: Vec<u8>,
}

/// A complete SNES machine.
#[derive(Debug)]
pub struct Machine {
    cpu: Cpu,
    memory: Memory,
    standard: VideoStandard,
    frame_cycles: u64,
    clock: u64,
    frame: u64,
    last_line: u64,
    last_clock: u64,
    trace: VecDeque<CpuTrace>,
    trace_limit: usize,
}

impl Machine {
    /// Build a machine around a cartridge and run the reset sequence.
    pub fn new(cartridge: Cartridge) -> Result<Self, MachineError> {
        if cartridge.needs_coprocessor() {
            return Err(MachineError::Coprocessor(cartridge.info().coprocessor));
        }

        let standard = cartridge.info().video_standard;
        let frame_cycles = standard.master_cycles_per_frame();

        let mut machine = Self {
            cpu: Cpu::new(),
            memory: Memory::new(cartridge, standard),
            standard,
            frame_cycles,
            clock: 0,
            frame: 0,
            last_line: 0,
            last_clock: 0,
            trace: VecDeque::new(),
            trace_limit: 0,
        };
        machine.reset();

        Ok(machine)
    }

    /// Reset the CPU and the frame clock, keeping the cartridge and save RAM.
    pub fn reset(&mut self) {
        self.cpu.reset(&mut self.memory);
        self.memory.ppu_mut().reset_timing();
        self.memory.apu_mut().reset();
        self.clock = 0;
        self.frame = 0;
        self.last_line = 0;
        self.last_clock = 0;
    }

    /// CPU state.
    #[must_use]
    pub const fn cpu(&self) -> &Cpu {
        &self.cpu
    }

    /// Memory map.
    #[must_use]
    pub const fn memory(&self) -> &Memory {
        &self.memory
    }

    /// Mutable memory map, for save RAM and debugging.
    pub fn memory_mut(&mut self) -> &mut Memory {
        &mut self.memory
    }

    /// Video standard of the inserted cartridge.
    #[must_use]
    pub const fn video_standard(&self) -> VideoStandard {
        self.standard
    }

    /// Master cycles executed since reset.
    #[must_use]
    pub const fn clock(&self) -> u64 {
        self.clock
    }

    /// Frames completed since reset.
    #[must_use]
    pub const fn frame_count(&self) -> u64 {
        self.frame
    }

    /// Master cycles in one frame for this standard.
    #[must_use]
    pub const fn frame_cycles(&self) -> u64 {
        self.frame_cycles
    }

    /// Execute CPU instructions until the current frame ends.
    ///
    /// The NMI line is pulsed at the start of vertical blanking, exactly where
    /// games expect to run their per-frame logic.
    pub fn run_frame(&mut self) -> FrameResult {
        loop {
            self.memory.ppu_mut().advance_to(self.clock);
            self.run_hdma();

            let delta = self.clock - self.last_clock;
            if delta > 0 {
                self.last_clock = self.clock;
                self.memory.apu_mut().advance(delta);
            }

            if self.memory.ppu_mut().frame_ready() {
                self.memory.ppu_mut().clear_frame_ready();
                break;
            }

            let before = self.cpu.cycles();
            if self.trace_limit > 0 {
                let registers = self.cpu.registers();
                let mapped = self.memory.peek(registers.pbr, registers.pc);
                let _ = &mapped;
                let opcode = mapped.unwrap_or(0);
                self.trace.push_back(CpuTrace {
                    pbr: registers.pbr,
                    pc: registers.pc,
                    opcode,
                    a: registers.a,
                    x: registers.x,
                    y: registers.y,
                    sp: registers.s,
                    p: registers.p,
                });
                while self.trace.len() > self.trace_limit {
                    self.trace.pop_front();
                }
                if mapped.is_none() {
                    // Freeze the ring at the first unmapped fetch.
                    self.trace_limit = 0;
                }
            }

            self.cpu.step(&mut self.memory);
            let elapsed = self.cpu.cycles() - before;

            // A stopped CPU consumes no cycles; keep time moving anyway so the
            // frame loop cannot spin forever.
            self.clock += if elapsed == 0 {
                MASTER_CYCLES_PER_CPU_CYCLE
            } else {
                elapsed * MASTER_CYCLES_PER_CPU_CYCLE
            };
            self.clock += self.memory.take_pending_cycles();

            if self.memory.ppu_mut().take_nmi() {
                self.memory.note_nmi();
                if self.memory.nmi_enabled() {
                    self.cpu.set_nmi(true);
                }
            }
        }

        self.frame += 1;

        FrameResult {
            frame: self.frame - 1,
            clock: self.clock,
            hash: self.state_hash(),
        }
    }

    /// Run HDMA for every scanline the PPU crossed since the last call.
    fn run_hdma(&mut self) {
        let line = self.memory.ppu().line_counter();
        let lines = u64::from(self.memory.ppu().timing().lines_per_frame);
        let visible = u64::from(self.memory.ppu().visible_lines());

        while self.last_line < line {
            self.last_line += 1;
            let within = self.last_line % lines;
            if within < visible {
                self.memory.run_hdma_line(within as u16);
            }
        }

        self.clock += self.memory.take_pending_cycles();
    }

    /// Record the last `limit` CPU instructions.
    pub fn set_cpu_trace(&mut self, limit: usize) {
        self.trace_limit = limit;
        self.trace.clear();
    }

    /// Recorded CPU instructions, oldest first.
    #[must_use]
    pub fn cpu_trace(&self) -> Vec<CpuTrace> {
        self.trace.iter().copied().collect()
    }

    /// Mixed stereo audio produced so far.
    #[must_use]
    pub fn audio(&self) -> &[i16] {
        self.memory.apu().audio()
    }

    /// Take the accumulated audio samples.
    pub fn take_audio(&mut self) -> Vec<i16> {
        self.memory.apu_mut().take_audio()
    }

    /// Fingerprint of the current machine state.
    ///
    /// FNV-1a over the CPU registers, WRAM and save RAM. Once the PPU exists,
    /// the framebuffer joins the hash.
    #[must_use]
    pub fn state_hash(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;

        let registers = self.cpu.registers();
        for byte in [
            registers.a as u8,
            (registers.a >> 8) as u8,
            registers.x as u8,
            (registers.x >> 8) as u8,
            registers.y as u8,
            (registers.y >> 8) as u8,
            registers.s as u8,
            (registers.s >> 8) as u8,
            registers.d as u8,
            (registers.d >> 8) as u8,
            registers.pc as u8,
            (registers.pc >> 8) as u8,
            registers.pbr,
            registers.dbr,
            registers.p,
            u8::from(registers.e),
        ] {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }

        for byte in self
            .memory
            .wram()
            .iter()
            .chain(self.memory.cartridge().sram())
        {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }

        hash
    }

    /// Serialize the full machine state.
    pub fn save_state(&self) -> Result<Vec<u8>, StateError> {
        let blob = StateBlob {
            version: STATE_VERSION,
            clock: self.clock,
            frame: self.frame,
            cpu: self.cpu.clone(),
            ppu: self.memory.ppu().clone(),
            apu: self.memory.apu().clone(),
            wram: self.memory.wram().to_vec(),
            sram: self.memory.cartridge().sram().to_vec(),
        };

        bincode::serde::encode_to_vec(&blob, bincode::config::standard())
            .map_err(|error| StateError::Encode(error.to_string()))
    }

    /// Restore a machine state produced by [`Machine::save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        let (blob, _): (StateBlob, usize) =
            bincode::serde::decode_from_slice(bytes, bincode::config::standard())
                .map_err(|error| StateError::Decode(error.to_string()))?;

        if blob.version != STATE_VERSION {
            return Err(StateError::Version {
                expected: STATE_VERSION,
                found: blob.version,
            });
        }

        self.cpu = blob.cpu;
        self.clock = blob.clock;
        self.frame = blob.frame;
        *self.memory.ppu_mut() = blob.ppu;
        *self.memory.apu_mut() = blob.apu;
        self.memory.wram_mut().copy_from_slice(&blob.wram);
        self.memory.cartridge_mut().load_sram(&blob.sram);

        Ok(())
    }

    /// Run a single CPU instruction. Useful for tests and debugging.
    pub fn step(&mut self) {
        let before = self.cpu.cycles();
        self.cpu.step(&mut self.memory);
        let elapsed = self.cpu.cycles() - before;
        self.clock += if elapsed == 0 {
            MASTER_CYCLES_PER_CPU_CYCLE
        } else {
            elapsed * MASTER_CYCLES_PER_CPU_CYCLE
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use emo_cpu::Bus;

    use crate::cartridge::Mapping;

    const BASE: usize = 0x7FC0;

    /// LoROM image running: LDA #$42; STA $00; JMP self.
    fn bootable_rom() -> Cartridge {
        let mut rom = vec![0u8; 64 * 1024];

        // LDA #$42 ; STA $00 ; JMP $8004
        rom[..9].copy_from_slice(&[0xA9, 0x42, 0x85, 0x00, 0x4C, 0x04, 0x80, 0xEA, 0xEA]);

        rom[BASE..BASE + 4].copy_from_slice(b"BOOT");
        rom[BASE + 0x15] = 0x20;
        rom[BASE + 0x16] = 0x00;
        rom[BASE + 0x17] = 6;
        rom[BASE + 0x18] = 1;
        rom[BASE + 0x19] = 1;
        rom[BASE + 0x3C] = 0x00; // reset vector -> $8000
        rom[BASE + 0x3D] = 0x80;

        Cartridge::from_rom(rom).expect("valid cartridge")
    }

    #[test]
    fn boots_and_runs_a_program() {
        let mut machine = Machine::new(bootable_rom()).expect("machine");

        let result = machine.run_frame();

        assert_eq!(result.frame, 0);
        assert_eq!(machine.memory_mut().read(0x00, 0x0000), 0x42);
        assert_eq!(machine.frame_count(), 1);
        assert!(machine.clock() >= machine.frame_cycles());
    }

    #[test]
    fn frames_are_deterministic() {
        let mut first = Machine::new(bootable_rom()).expect("machine");
        let mut second = Machine::new(bootable_rom()).expect("machine");

        let a = first.run_frame();
        let b = second.run_frame();

        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn save_states_restore_identical_state() {
        let mut machine = Machine::new(bootable_rom()).expect("machine");
        machine.run_frame();
        let saved = machine.save_state().expect("save");
        let hash = machine.state_hash();
        let clock = machine.clock();

        machine.run_frame();
        machine.load_state(&saved).expect("load");

        assert_eq!(machine.state_hash(), hash);
        assert_eq!(machine.clock(), clock);
    }

    #[test]
    fn rejects_coprocessor_cartridges() {
        let mut cart = bootable_rom();
        // Rebuild with a SuperFX type byte.
        let mut rom = cart.rom().to_vec();
        rom[BASE + 0x16] = 0x13;
        cart = Cartridge::from_rom(rom).expect("valid cartridge");

        assert!(matches!(
            Machine::new(cart),
            Err(MachineError::Coprocessor(Coprocessor::SuperFx))
        ));
    }

    #[test]
    fn pal_cartridges_use_pal_timing() {
        let mut rom = bootable_rom().rom().to_vec();
        rom[BASE + 0x19] = 0x02;
        let cart = Cartridge::from_rom(rom).expect("valid cartridge");
        let machine = Machine::new(cart).expect("machine");

        assert_eq!(machine.video_standard(), VideoStandard::Pal);
        assert_eq!(machine.frame_cycles(), 425_568);
        assert_eq!(machine.memory().cartridge().info().mapping, Mapping::LoRom);
    }
}
