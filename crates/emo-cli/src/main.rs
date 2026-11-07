//! `emo` command-line entry point.
//!
//! The binary exposes the machine's timing model and the instruction vector
//! runner. The headless emulator runner arrives with the machine milestone.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use emo_core::{Cartridge, Machine, VideoStandard};

mod spc_vectors;
mod vectors;

/// Top-level command-line interface.
#[derive(Debug, Parser)]
#[command(name = "emo", version, about = "A from-scratch SNES emulator.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
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
        /// Write the final 64 KiB of VRAM to a file.
        #[arg(long)]
        dump_vram: Option<PathBuf>,
        /// Write the final 128 KiB of WRAM to a file.
        #[arg(long)]
        dump_wram: Option<PathBuf>,
        /// Write the mixed stereo audio as raw signed 16-bit little-endian.
        #[arg(long)]
        dump_audio: Option<PathBuf>,
        /// Write the final 64 KiB of APU RAM to a file.
        #[arg(long)]
        dump_aram: Option<PathBuf>,
        /// Write the final 544 bytes of OAM to a file.
        #[arg(long)]
        dump_oam: Option<PathBuf>,
        /// Write the final 512 bytes of CGRAM to a file.
        #[arg(long)]
        dump_cgram: Option<PathBuf>,
        /// Write the current APU state as a `.spc` snapshot.
        #[arg(long)]
        dump_spc: Option<PathBuf>,
        /// Only record audio from this frame onwards.
        #[arg(long, default_value_t = 0)]
        audio_from: u64,
        /// Write the recorded SPC700 instructions to a text file.
        #[arg(long)]
        dump_spc_trace: Option<PathBuf>,
        /// Print CPU and APU state after the run.
        #[arg(long)]
        debug: bool,
        /// Record this many SPC700 instructions after boot.
        #[arg(long, default_value_t = 0)]
        spc_trace: usize,
        /// Record the last N main-CPU instructions.
        #[arg(long, default_value_t = 0)]
        cpu_trace: usize,
        /// Load a 64-byte IPL ROM instead of the built-in boot protocol.
        #[arg(long)]
        ipl: Option<PathBuf>,
        /// Hold a controller button for eight frames (`start@600`).
        #[arg(long = "press", value_name = "BUTTON@FRAME")]
        press: Vec<String>,
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
    /// Run SingleStepTests/spc700 vectors against the SPC700 core.
    VerifySpc {
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
        Command::VerifySpc { path } => verify_spc(&path),
        Command::Run {
            rom,
            frames,
            hash,
            sram,
            save_state,
            dump_frame,
            dump_vram,
            dump_wram,
            dump_audio,
            dump_aram,
            dump_oam,
            dump_cgram,
            dump_spc,
            audio_from,
            dump_spc_trace,
            debug,
            spc_trace,
            cpu_trace,
            ipl,
            press,
        } => run(
            &rom,
            frames,
            hash,
            sram.as_ref(),
            save_state.as_ref(),
            dump_frame.as_ref(),
            dump_vram.as_ref(),
            dump_wram.as_ref(),
            dump_audio.as_ref(),
            dump_aram.as_ref(),
            dump_oam.as_ref(),
            dump_cgram.as_ref(),
            dump_spc.as_ref(),
            audio_from,
            dump_spc_trace.as_ref(),
            debug,
            spc_trace,
            cpu_trace,
            ipl.as_ref(),
            &press,
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
    dump_vram_path: Option<&PathBuf>,
    dump_wram_path: Option<&PathBuf>,
    dump_audio_path: Option<&PathBuf>,
    dump_aram_path: Option<&PathBuf>,
    dump_oam_path: Option<&PathBuf>,
    dump_cgram_path: Option<&PathBuf>,
    dump_spc_path: Option<&PathBuf>,
    audio_from: u64,
    dump_spc_trace_path: Option<&PathBuf>,
    debug: bool,
    spc_trace: usize,
    cpu_trace: usize,
    ipl_path: Option<&PathBuf>,
    press: &[String],
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

    let ipl_path = ipl_path.map(PathBuf::from).or_else(|| {
        ["roms/ipl.bin", "ipl.bin"]
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
    });

    if let Some(ipl_path) = ipl_path {
        match std::fs::read(&ipl_path) {
            Ok(bytes) => {
                machine.memory_mut().apu_mut().load_ipl(&bytes);
                eprintln!("using IPL ROM {}", ipl_path.display());
            }
            Err(error) => {
                eprintln!("failed to read {}: {error}", ipl_path.display());
                return ExitCode::FAILURE;
            }
        }
    } else {
        eprintln!(
            "no IPL ROM found; using the experimental built-in boot protocol (place a 64-byte ipl.bin in roms/ to use the real one)"
        );
    }

    if spc_trace > 0 {
        machine.memory_mut().apu_mut().set_spc_trace(spc_trace);
    }
    if cpu_trace > 0 {
        machine.set_cpu_trace(cpu_trace);
    }

    let mut presses = Vec::new();
    for spec in press {
        let Some((name, frame)) = spec.split_once('@') else {
            eprintln!("invalid --press {spec:?}; expected BUTTON@FRAME");
            return ExitCode::FAILURE;
        };
        let Some(button) = button_mask(name) else {
            eprintln!("unknown button {name:?}");
            return ExitCode::FAILURE;
        };
        let Ok(frame) = frame.parse::<u64>() else {
            eprintln!("invalid frame in --press {spec:?}");
            return ExitCode::FAILURE;
        };
        presses.push((frame, button));
    }

    let mut last_hash = 0;
    let mut audio = Vec::new();
    for frame in 0..frames {
        let buttons = presses
            .iter()
            .filter(|(start, _)| frame >= *start && frame < start + 8)
            .fold(0u16, |acc, (_, button)| acc | button);
        machine.memory_mut().set_buttons(0, buttons);

        let result = machine.run_frame();
        last_hash = result.hash;
        if dump_audio_path.is_some() {
            if frame >= audio_from {
                audio.extend(machine.take_audio());
            } else {
                machine.take_audio();
            }
        }
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
        println!(
            "nmitimen={:02x} inidisp={:02x} tm={:02x} htime={} vtime={} irq={} line={} vblank={} nmi={}",
            machine.memory().nmitimen(),
            machine.memory().ppu().display_control(),
            machine.memory().ppu().main_screen(),
            machine.memory().htime(),
            machine.memory().vtime(),
            machine.memory().irq_pending(),
            machine.memory().ppu().line_counter(),
            machine.memory().ppu().in_vblank(),
            machine.memory().ppu().nmi_pending()
        );
        {
            let ppu = machine.memory().ppu();
            println!(
                "ppu mode={} tm={:02x} ts={:02x} bgsc={:02x?} char={:04x?} inidisp={:02x} obsel={:02x} vmain={:02x}",
                ppu.background_mode(),
                ppu.main_screen(),
                ppu.sub_screen(),
                [
                    ppu.background_screen(0),
                    ppu.background_screen(1),
                    ppu.background_screen(2),
                    ppu.background_screen(3)
                ],
                [
                    ppu.background_char_base(0),
                    ppu.background_char_base(1),
                    ppu.background_char_base(2),
                    ppu.background_char_base(3)
                ],
                ppu.display_control(),
                ppu.obsel(),
                ppu.vmain()
            );
        }

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

        for record in apu.spc_trace().iter().rev().take(30).rev() {
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

    if let Some(dump_vram_path) = dump_vram_path
        && let Err(error) = std::fs::write(dump_vram_path, machine.memory().ppu().vram())
    {
        eprintln!("failed to write {}: {error}", dump_vram_path.display());
        return ExitCode::FAILURE;
    }

    if let Some(dump_wram_path) = dump_wram_path
        && let Err(error) = std::fs::write(dump_wram_path, machine.memory().wram())
    {
        eprintln!("failed to write {}: {error}", dump_wram_path.display());
        return ExitCode::FAILURE;
    }

    if let Some(dump_oam_path) = dump_oam_path
        && let Err(error) = std::fs::write(dump_oam_path, &machine.memory().ppu().oam()[..0x220])
    {
        eprintln!("failed to write {}: {error}", dump_oam_path.display());
        return ExitCode::FAILURE;
    }

    if let Some(dump_cgram_path) = dump_cgram_path {
        let cgram = machine.memory().ppu().cgram();
        let mut bytes = Vec::with_capacity(cgram.len() * 2);
        for value in cgram {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        if let Err(error) = std::fs::write(dump_cgram_path, bytes) {
            eprintln!("failed to write {}: {error}", dump_cgram_path.display());
            return ExitCode::FAILURE;
        }
    }

    if let Some(dump_spc_path) = dump_spc_path {
        let apu = machine.memory().apu();
        let spc = apu.spc();
        let mut blob = vec![0u8; 0x10200];
        let signature = b"SNES-SPC700 Sound File Data v0.30";
        blob[..signature.len()].copy_from_slice(signature);
        blob[0x1B] = 0x1A;
        blob[0x1C] = 0x1A;
        blob[0x1D] = spc.pc as u8;
        blob[0x1E] = (spc.pc >> 8) as u8;
        blob[0x1F] = spc.a;
        blob[0x20] = spc.x;
        blob[0x21] = spc.y;
        blob[0x22] = spc.psw;
        blob[0x23] = spc.sp;
        blob[0x100..0x10100].copy_from_slice(apu.aram());
        blob[0x100 + 0xF0..0x100 + 0x100].copy_from_slice(&apu.smp_registers());
        blob[0x10100..0x10180].copy_from_slice(apu.dsp().raw_registers());
        if let Err(error) = std::fs::write(dump_spc_path, blob) {
            eprintln!("failed to write {}: {error}", dump_spc_path.display());
            return ExitCode::FAILURE;
        }
    }

    if let Some(dump_audio_path) = dump_audio_path {
        let mut bytes = Vec::with_capacity(audio.len() * 2);
        for sample in &audio {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        if let Err(error) = std::fs::write(dump_audio_path, bytes) {
            eprintln!("failed to write {}: {error}", dump_audio_path.display());
            return ExitCode::FAILURE;
        }
    }

    if let Some(dump_aram_path) = dump_aram_path
        && let Err(error) = std::fs::write(dump_aram_path, machine.memory().apu().aram())
    {
        eprintln!("failed to write {}: {error}", dump_aram_path.display());
        return ExitCode::FAILURE;
    }

    if let Some(dump_spc_trace_path) = dump_spc_trace_path {
        let mut text = String::new();
        for record in machine.memory().apu().spc_trace() {
            text.push_str(&format!(
                "{:04x} {:02x} -> {:04x} a={:02x} x={:02x} y={:02x} psw={:02x} sp={:02x}
",
                record.pc,
                record.opcode,
                record.next_pc,
                record.a,
                record.x,
                record.y,
                record.psw,
                record.sp
            ));
        }
        if let Err(error) = std::fs::write(dump_spc_trace_path, text) {
            eprintln!("failed to write {}: {error}", dump_spc_trace_path.display());
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

fn verify_spc(path: &PathBuf) -> ExitCode {
    let files = match vector_files(path) {
        Ok(files) => files,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let mut total = spc_vectors::Report::default();
    for file in files {
        match spc_vectors::run_file(&file) {
            Ok(report) => {
                if report.failed > 0 {
                    println!(
                        "{}: {}/{} passed, {} failed",
                        file.file_name().unwrap_or_default().to_string_lossy(),
                        report.passed,
                        report.passed + report.failed,
                        report.failed
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

fn button_mask(name: &str) -> Option<u16> {
    Some(match name.to_ascii_lowercase().as_str() {
        "b" => 0x8000,
        "y" => 0x4000,
        "select" => 0x2000,
        "start" => 0x1000,
        "up" => 0x0800,
        "down" => 0x0400,
        "left" => 0x0200,
        "right" => 0x0100,
        "a" => 0x0080,
        "x" => 0x0040,
        "l" => 0x0020,
        "r" => 0x0010,
        _ => return None,
    })
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
