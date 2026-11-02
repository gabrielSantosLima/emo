//! `emo` command-line entry point.
//!
//! The binary exposes the machine's timing model and the instruction vector
//! runner. The headless emulator runner arrives with the machine milestone.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use emo_core::VideoStandard;

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
        Command::Verify { path } => verify(&path),
    }
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
