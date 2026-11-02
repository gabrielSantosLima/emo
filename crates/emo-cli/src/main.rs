//! `emo` command-line entry point.
//!
//! The binary exposes the machine's timing model and the instruction vector
//! runner. The headless emulator runner arrives with the machine milestone.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use emo_core::{Cartridge, Machine, VideoStandard};

mod vectors;

/// Top-level command-line interface.
#[derive(Debug, Parser)]
#[command(name = "emo", version, about = "A from-scratch SNES emulator.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the master-clock timing of a video standard.
    Timing {
        /// Video standard to describe.
        #[arg(value_enum, default_value_t = StandardArg::Ntsc)]
        standard: StandardArg,
    },
    /// Run a cartridge headlessly and print a per-frame report.
    Run {
        /// ROM image to run.
        rom: PathBuf,
        /// Number of frames to execute.
        #[arg(long, default_value_t = 60)]
        frames: u64,
        /// Print a hash line for every frame.
        #[arg(long)]
        hash: bool,
        /// Load and save battery-backed SRAM at this path.
        #[arg(long)]
        sram: Option<PathBuf>,
        /// Write the final save state to this path.
        #[arg(long)]
        save_state: Option<PathBuf>,
        /// Write the final frame as a PPM image.
        #[arg(long)]
        dump_frame: Option<PathBuf>,
        /// Print CPU and APU state after the run.
        #[arg(long)]
        debug: bool,
        /// Record this many SPC700 instructions after boot.
        #[arg(long, default_value_t = 0)]
        spc_trace: usize,
        /// Record the last N main-CPU instructions.
        #[arg(long, default_value_t = 0)]
        cpu_trace: usize,
    },
    /// Print the decoded header of a cartridge image.
    Info {
        /// ROM image to inspect.
        rom: PathBuf,
    },
    /// Run SingleStepTests/65816 vectors against the CPU core.
    Verify {
        /// A vector JSON file or a directory of vector files.
        path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum StandardArg {
    #[default]
    Ntsc,
    Pal,
}

impl From<StandardArg> for VideoStandard {
    fn from(value: StandardArg) -> Self {
        match value {
            StandardArg::Ntsc => Self::Ntsc,
            StandardArg::Pal => Self::Pal,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Timing { standard } => {
            print!("{}", timing_report(standard.into()));
            ExitCode::SUCCESS
        }
        Command::Run {
            rom,
            frames,
            hash,
            sram,
            save_state,
            dump_frame,
            debug,
            spc_trace,
            cpu_trace,
        } => run(
            &rom,
            frames,
            hash,
            sram.as_ref(),
            save_state.as_ref(),
            dump_frame.as_ref(),
            debug,
            spc_trace,
            cpu_trace,
        ),
        Command::Info { rom } => info(&rom),
        Command::Verify { path } => verify(&path),
    }
}

/// Run a cartridge for a fixed number of frames.
#[allow(clippy::too_many_arguments)]
fn run(
    path: &PathBuf,
    frames: u64,
    hash: bool,
    sram_path: Option<&PathBuf>,
    save_state_path: Option<&PathBuf>,
    dump_frame_path: Option<&PathBuf>,
    debug: bool,
    spc_trace: usize,
    cpu_trace: usize,
) -> ExitCode {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("failed to read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let mut cartridge = match Cartridge::from_rom(bytes) {
        Ok(cartridge) => cartridge,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(sram_path) = sram_path
        && let Ok(data) = std::fs::read(sram_path)
    {
        cartridge.load_sram(&data);
    }

    let mut machine = match Machine::new(cartridge) {
        Ok(machine) => machine,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    if spc_trace > 0 {
        machine.memory_mut().apu_mut().set_spc_trace(spc_trace);
    }
    if cpu_trace > 0 {
        machine.set_cpu_trace(cpu_trace);
    }

    let mut last_hash = 0;
    for _ in 0..frames {
        let result = machine.run_frame();
        last_hash = result.hash;
        if hash {
            println!(
                "frame {:6}  clock {:10}  hash {:016x}",
                result.frame, result.clock, result.hash
            );
        }
    }

    println!(
        "ran {} frame(s)  clock {}  hash {:016x}",
        machine.frame_count(),
        machine.clock(),
        last_hash
    );

    if let Some(save_state_path) = save_state_path {
        match machine.save_state() {
            Ok(state) => {
                if let Err(error) = std::fs::write(save_state_path, state) {
                    eprintln!("failed to write {}: {error}", save_state_path.display());
                    return ExitCode::FAILURE;
                }
            }
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    }

    if debug {
        let cpu = machine.cpu().registers();
        let apu = machine.memory().apu();
        println!(
            "cpu pc={:#06x} pbr={:#04x} p={:#04x}  apu booting={} spc pc={:#06x} a={:#04x} x={:#04x} y={:#04x} psw={:#04x}",
            cpu.pc,
            cpu.pbr,
            cpu.p,
            apu.is_booting(),
            apu.spc().pc,
            apu.spc().a,
            apu.spc().x,
            apu.spc().y,
            apu.spc().psw
        );
        println!("port writes: {:02x?}", apu.port_trace());
        let writes = apu.cpu_port_writes();
        let reads = apu.spc_port_reads();
        println!(
            "cpu port writes (last 12 of {}): {:?}",
            writes.len(),
            &writes[writes.len().saturating_sub(12)..]
        );
        println!(
            "spc port reads (last 8 of {}): {:?}",
            reads.len(),
            &reads[reads.len().saturating_sub(8)..]
        );
        println!("boot log: {:04x?}", apu.boot_log());
        let writes = apu.boot_writes();
        println!("boot writes ({}): {:02x?}", writes.len(), writes);
        let aram = apu.aram();
        println!("aram 09e0: {:02x?}", &aram[0x09E0..0x0A00]);
        println!("aram 0500: {:02x?}", &aram[0x0500..0x0520]);
        println!("aram 0550: {:02x?}", &aram[0x0550..0x0570]);
        println!("aram 1360: {:02x?}", &aram[0x1360..0x1440]);
        println!("ipl accept tail: {:04x?}", apu.boot_accept_tail());
        println!("ipl blocks: {:04x?}", apu.boot_blocks());
        println!("ipl rejects: {:?}", apu.boot_rejects());
        let decisions = apu.boot_decisions();
        println!(
            "ipl decisions (last 32 of {}): {:?}",
            decisions.len(),
            &decisions[decisions.len().saturating_sub(32)..]
        );
        println!("aram ffde: {:02x?}", &aram[0xFFC0..0xFFE0]);

        for record in machine.cpu_trace() {
            println!(
                "cpu {:02x}:{:04x}: {:02x}  a={:04x} x={:04x} y={:04x} sp={:04x} p={:02x}",
                record.pbr,
                record.pc,
                record.opcode,
                record.a,
                record.x,
                record.y,
                record.sp,
                record.p
            );
        }

        let spc_trace = apu.spc_trace();
        for record in spc_trace.iter().rev().take(5000).rev() {
            println!(
                "spc {:04x}: {:02x} -> {:04x}  a={:02x} x={:02x} y={:02x} psw={:02x} sp={:02x}",
                record.pc,
                record.opcode,
                record.next_pc,
                record.a,
                record.x,
                record.y,
                record.psw,
                record.sp
            );
        }
    }

    if let Some(dump_frame_path) = dump_frame_path {
        let ppu = machine.memory().ppu();
        let height = usize::from(ppu.timing().visible_lines);
        let mut image = format!(
            "P6
256 {height}
255
"
        )
        .into_bytes();
        image.extend_from_slice(ppu.framebuffer());

        if let Err(error) = std::fs::write(dump_frame_path, image) {
            eprintln!("failed to write {}: {error}", dump_frame_path.display());
            return ExitCode::FAILURE;
        }
    }

    if let Some(sram_path) = sram_path
        && let Err(error) = std::fs::write(sram_path, machine.memory().cartridge().sram())
    {
        eprintln!("failed to write {}: {error}", sram_path.display());
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// Print the cartridge header of `path`.
fn info(path: &PathBuf) -> ExitCode {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("failed to read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let cartridge = match Cartridge::from_rom(bytes) {
        Ok(cartridge) => cartridge,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let info = cartridge.info();
    let standard = match info.video_standard {
        VideoStandard::Ntsc => "NTSC",
        VideoStandard::Pal => "PAL",
    };

    println!("title       {}", info.title);
    println!("mapping     {}", info.mapping);
    println!("rom size    {} bytes", info.rom_size);
    println!("sram size   {} bytes", info.sram_size);
    println!("region      {:#04x} ({standard})", info.region);
    println!(
        "checksum    {:#06x} ({})",
        info.checksum,
        if info.checksum_valid {
            "valid"
        } else {
            "invalid"
        }
    );
    println!("version     {:#04x}", info.version);
    println!("coprocessor {}", info.coprocessor);

    if cartridge.needs_coprocessor() {
        eprintln!(
            "warning: {} is not implemented; this cartridge will not run",
            info.coprocessor
        );
    }

    ExitCode::SUCCESS
}

/// Run every vector file under `path` and report divergences.
fn verify(path: &PathBuf) -> ExitCode {
    let files = match vector_files(path) {
        Ok(files) => files,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    if files.is_empty() {
        eprintln!("no .json vector files found in {}", path.display());
        return ExitCode::FAILURE;
    }

    let mut total = vectors::Report::default();

    for file in &files {
        match vectors::run_file(file) {
            Ok(report) => {
                let cases = report.passed + report.failed;
                let name = file.file_name().map_or_else(
                    || file.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                if report.failed == 0 {
                    println!("{name}: {}/{} passed", report.passed, cases);
                } else {
                    println!(
                        "{name}: {}/{} passed, {} failed",
                        report.passed, cases, report.failed
                    );
                    for failure in &report.failures {
                        println!("  {failure}");
                    }
                }
                total.merge(report);
            }
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        }
    }

    println!("total: {} passed, {} failed", total.passed, total.failed);

    if total.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn vector_files(path: &PathBuf) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.clone()]);
    }

    let entries = std::fs::read_dir(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;

    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| {
            entry
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();

    Ok(files)
}

/// Render the timing table for one video standard.
///
/// The frame rate is derived from the master clock and the integer line/frame
/// counters, never hard-coded, so the output always agrees with the scheduler.
fn timing_report(standard: VideoStandard) -> String {
    let cycles_per_frame = standard.master_cycles_per_frame();
    let frame_rate = standard.master_clock() as f64 / cycles_per_frame as f64;
    let label = match standard {
        VideoStandard::Ntsc => "NTSC",
        VideoStandard::Pal => "PAL",
    };

    let rows = [
        ("master clock", format!("{} Hz", standard.master_clock())),
        (
            "scanlines per frame",
            standard.lines_per_frame().to_string(),
        ),
        (
            "cycles per line",
            emo_core::timing::MASTER_CYCLES_PER_LINE.to_string(),
        ),
        ("cycles per frame", cycles_per_frame.to_string()),
        ("frame rate", format!("{frame_rate:.4} Hz")),
    ];

    let mut report = format!("{label} timing\n");
    for (name, value) in rows {
        report.push_str(&format!("  {name:<20}{value}\n"));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntsc_report_uses_the_frame_budget() {
        let report = timing_report(VideoStandard::Ntsc);

        assert!(report.contains("NTSC"));
        assert!(report.contains("357368"));
        assert!(report.contains("60.0985"));
    }

    #[test]
    fn pal_report_uses_the_frame_budget() {
        let report = timing_report(VideoStandard::Pal);

        assert!(report.contains("PAL"));
        assert!(report.contains("425568"));
        assert!(report.contains("50.0070"));
    }
}
