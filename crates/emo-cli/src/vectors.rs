//! Runner for the `SingleStepTests/65816` instruction vectors.
//!
//! Each vector file contains one instruction's worth of randomized cases: full
//! register state before and after, the RAM bytes that matter, and the exact
//! bus cycle count. The runner executes one instruction per case on a flat
//! 16 MB address space and compares the architectural state.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use emo_cpu::{Bus, Cpu, FlatMemory, Registers};
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
    s: u16,
    p: u8,
    a: u16,
    x: u16,
    y: u16,
    dbr: u8,
    d: u16,
    pbr: u8,
    e: u8,
    ram: Vec<[u32; 2]>,
}

fn bank_offset(address: u32) -> (u8, u16) {
    ((address >> 16) as u8, address as u16)
}

/// Run every case in a JSON vector file.
pub fn run_file(path: &Path) -> Result<Report, Error> {
    let bytes = fs::read(path).map_err(|error| Error::Read(path.to_path_buf(), error))?;
    let vectors: Vec<Vector> =
        serde_json::from_slice(&bytes).map_err(|error| Error::Parse(path.to_path_buf(), error))?;

    let mut memory = FlatMemory::new();
    let mut cpu = Cpu::new();
    let mut report = Report::default();
    let mut touched: Vec<u32> = Vec::new();

    for vector in &vectors {
        for [address, value] in &vector.initial.ram {
            let (bank, offset) = bank_offset(*address);
            memory.write(bank, offset, *value as u8);
            touched.push(*address);
        }

        cpu.set_registers(Registers {
            a: vector.initial.a,
            x: vector.initial.x,
            y: vector.initial.y,
            s: vector.initial.s,
            d: vector.initial.d,
            pc: vector.initial.pc,
            pbr: vector.initial.pbr,
            dbr: vector.initial.dbr,
            p: vector.initial.p,
            e: vector.initial.e != 0,
        });
        cpu.set_cycles(0);

        cpu.step(&mut memory);

        let mut problems = Vec::new();
        compare_registers(&cpu, &vector.final_state, &mut problems);
        compare_memory(&mut memory, &vector.final_state.ram, &mut problems);

        let expected_cycles = vector.cycles.len() as u64;
        if cpu.cycles() != expected_cycles {
            problems.push(format!(
                "cycles: expected {expected_cycles}, got {}",
                cpu.cycles()
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

        for [address, _] in &vector.final_state.ram {
            let (bank, offset) = bank_offset(*address);
            memory.write(bank, offset, 0);
            touched.push(*address);
        }
        for address in touched.drain(..) {
            let (bank, offset) = bank_offset(address);
            memory.write(bank, offset, 0);
        }
    }

    Ok(report)
}

fn compare_registers(cpu: &Cpu, expected: &Snapshot, problems: &mut Vec<String>) {
    let actual = cpu.registers();

    if actual.pc != expected.pc {
        problems.push(format!(
            "pc: expected {:#06x}, got {:#06x}",
            expected.pc, actual.pc
        ));
    }
    if actual.s != expected.s {
        problems.push(format!(
            "s: expected {:#06x}, got {:#06x}",
            expected.s, actual.s
        ));
    }
    if actual.a != expected.a {
        problems.push(format!(
            "a: expected {:#06x}, got {:#06x}",
            expected.a, actual.a
        ));
    }
    if actual.x != expected.x {
        problems.push(format!(
            "x: expected {:#06x}, got {:#06x}",
            expected.x, actual.x
        ));
    }
    if actual.y != expected.y {
        problems.push(format!(
            "y: expected {:#06x}, got {:#06x}",
            expected.y, actual.y
        ));
    }
    if actual.d != expected.d {
        problems.push(format!(
            "d: expected {:#06x}, got {:#06x}",
            expected.d, actual.d
        ));
    }
    if actual.dbr != expected.dbr {
        problems.push(format!(
            "dbr: expected {:#04x}, got {:#04x}",
            expected.dbr, actual.dbr
        ));
    }
    if actual.pbr != expected.pbr {
        problems.push(format!(
            "pbr: expected {:#04x}, got {:#04x}",
            expected.pbr, actual.pbr
        ));
    }
    if actual.p != expected.p {
        problems.push(format!(
            "p: expected {:#04x}, got {:#04x}",
            expected.p, actual.p
        ));
    }
    if actual.e != (expected.e != 0) {
        problems.push(format!("e: expected {}, got {}", expected.e != 0, actual.e));
    }
}

fn compare_memory(memory: &mut FlatMemory, expected: &[[u32; 2]], problems: &mut Vec<String>) {
    for [address, value] in expected {
        let (bank, offset) = bank_offset(*address);
        let actual = memory.read(bank, offset);
        if actual != *value as u8 {
            problems.push(format!(
                "{address:#08x}: expected {value:#04x}, got {actual:#04x}"
            ));
        }
    }
}
