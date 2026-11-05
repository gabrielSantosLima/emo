//! Runner for the `SingleStepTests/spc700` instruction vectors.
//!
//! Each file holds 1000 randomized cases for one opcode. The format is the
//! same as the 65816 vectors but the address space is a flat 64 KiB and the
//! registers are the eight-bit SPC700 set.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use emo_apu::{ApuBus, Spc700};
use serde::Deserialize;

/// Failures reported per file before the runner stops collecting detail.
const MAX_REPORTED_FAILURES: usize = 10;

/// Errors raised while loading a vector file.
#[derive(Debug)]
pub enum Error {
    /// The file could not be read.
    Read(PathBuf, std::io::Error),
    /// The file is not a valid vector document.
    Parse(PathBuf, serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(path, error) => write!(f, "failed to read {}: {error}", path.display()),
            Self::Parse(path, error) => write!(f, "failed to parse {}: {error}", path.display()),
        }
    }
}

impl std::error::Error for Error {}

/// Summary of one vector file.
#[derive(Debug, Default)]
pub struct Report {
    /// Cases that matched the expected state, memory and cycle count.
    pub passed: usize,
    /// Cases that diverged.
    pub failed: usize,
    /// Human-readable descriptions of the first failures.
    pub failures: Vec<String>,
}

impl Report {
    /// Merge another report into this one.
    pub fn merge(&mut self, other: Self) {
        self.passed += other.passed;
        self.failed += other.failed;
        self.failures.extend(other.failures);
    }
}

/// Flat 64 KiB bus used by the vector runner.
struct FlatApuMemory {
    ram: Vec<u8>,
}

impl FlatApuMemory {
    fn new() -> Self {
        Self {
            ram: vec![0; 0x1_0000],
        }
    }
}

impl ApuBus for FlatApuMemory {
    fn read(&mut self, addr: u16) -> u8 {
        self.ram[usize::from(addr)]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.ram[usize::from(addr)] = value;
    }
}

#[derive(Deserialize)]
struct Vector {
    name: String,
    initial: Snapshot,
    #[serde(rename = "final")]
    final_state: Snapshot,
    cycles: Vec<(Option<u32>, Option<u8>, String)>,
}

#[derive(Deserialize)]
struct Snapshot {
    pc: u16,
    a: u8,
    x: u8,
    y: u8,
    sp: u8,
    psw: u8,
    ram: Vec<[u32; 2]>,
}

/// Run every case in a JSON vector file.
pub fn run_file(path: &Path) -> Result<Report, Error> {
    let bytes = fs::read(path).map_err(|error| Error::Read(path.to_path_buf(), error))?;
    let vectors: Vec<Vector> =
        serde_json::from_slice(&bytes).map_err(|error| Error::Parse(path.to_path_buf(), error))?;

    let mut memory = FlatApuMemory::new();
    let mut cpu = Spc700::new();
    let mut report = Report::default();

    for vector in &vectors {
        for [address, value] in &vector.initial.ram {
            memory.ram[*address as usize & 0xFFFF] = *value as u8;
        }

        cpu.pc = vector.initial.pc;
        cpu.a = vector.initial.a;
        cpu.x = vector.initial.x;
        cpu.y = vector.initial.y;
        cpu.sp = vector.initial.sp;
        cpu.psw = vector.initial.psw;
        cpu.stopped = false;

        let cycles = cpu.step(&mut memory);

        let mut problems = Vec::new();
        if cpu.pc != vector.final_state.pc {
            problems.push(format!(
                "pc: expected {:#06x}, got {:#06x}",
                vector.final_state.pc, cpu.pc
            ));
        }
        if cpu.a != vector.final_state.a {
            problems.push(format!(
                "a: expected {:#04x}, got {:#04x}",
                vector.final_state.a, cpu.a
            ));
        }
        if cpu.x != vector.final_state.x {
            problems.push(format!(
                "x: expected {:#04x}, got {:#04x}",
                vector.final_state.x, cpu.x
            ));
        }
        if cpu.y != vector.final_state.y {
            problems.push(format!(
                "y: expected {:#04x}, got {:#04x}",
                vector.final_state.y, cpu.y
            ));
        }
        if cpu.sp != vector.final_state.sp {
            problems.push(format!(
                "sp: expected {:#04x}, got {:#04x}",
                vector.final_state.sp, cpu.sp
            ));
        }
        if cpu.psw != vector.final_state.psw {
            problems.push(format!(
                "psw: expected {:#04x}, got {:#04x}",
                vector.final_state.psw, cpu.psw
            ));
        }
        for [address, value] in &vector.final_state.ram {
            let actual = memory.ram[*address as usize & 0xFFFF];
            if actual != *value as u8 {
                problems.push(format!(
                    "ram[{:#06x}]: expected {:#04x}, got {:#04x}",
                    address, value, actual
                ));
            }
        }
        if cycles as usize != vector.cycles.len() {
            problems.push(format!(
                "cycles: expected {}, got {cycles}",
                vector.cycles.len()
            ));
        }

        if problems.is_empty() {
            report.passed += 1;
        } else {
            report.failed += 1;
            if report.failures.len() < MAX_REPORTED_FAILURES {
                report
                    .failures
                    .push(format!("{}: {}", vector.name, problems.join("; ")));
            }
        }

        for [address, _] in &vector.initial.ram {
            memory.ram[*address as usize & 0xFFFF] = 0;
        }
        for [address, _] in &vector.final_state.ram {
            memory.ram[*address as usize & 0xFFFF] = 0;
        }
    }

    Ok(report)
}
