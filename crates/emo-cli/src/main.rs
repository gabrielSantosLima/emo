//! `emo` command-line entry point.
//!
//! The binary is intentionally small: it exposes the machine's timing model and
//! is the seed for the headless runner that arrives with the CPU milestone.

use clap::{Parser, Subcommand, ValueEnum};
use emo_core::VideoStandard;

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

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Timing { standard } => print!("{}", timing_report(standard.into())),
    }
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
