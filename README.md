<div align="center">

# emo

A from-scratch Super Nintendo Entertainment System (SNES) emulator.

[![CI](https://github.com/gabrielSantosLima/emo/actions/workflows/ci.yml/badge.svg)](https://github.com/gabrielSantosLima/emo/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/gabrielSantosLima/emo?label=release)](https://github.com/gabrielSantosLima/emo/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

## Status

Under active development. There are no playable builds yet: the Rust workspace,
tooling and CI are in place, and the emulator components are being implemented
milestone by milestone.

## Requirements

- Rust 1.94 or newer. The exact toolchain is pinned in `rust-toolchain.toml`.

## Building and testing

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo lint
```

`cargo lint` is an alias for `cargo clippy --workspace --all-targets -- -D warnings`.

## Inspecting a cartridge

```bash
cargo run -p emo-cli -- info path/to/game.sfc
```

Prints the decoded header: title, mapping, ROM/SRAM sizes, region, checksum,
version and coprocessor. Cartridges with an unimplemented coprocessor are
reported instead of silently misbehaving.

## Running headlessly

```bash
cargo run -p emo-cli -- run path/to/game.sfc --frames 60 --hash
```

Runs the cartridge without a display, pulses NMI at the start of vertical
blanking and prints the master clock plus a state hash per frame. Useful flags:

- `--frames N` - number of frames to execute (default 60).
- `--sram PATH` - load and save battery-backed SRAM at that path.
- `--save-state PATH` - write the final machine state (versioned, bincode).
- `--dump-frame PATH` - write the final frame as a PPM (P6) image.
- `--ipl PATH` - use a 64-byte SPC700 IPL ROM instead of the built-in boot
  protocol. If `roms/ipl.bin` or `ipl.bin` exists it is picked up automatically.
  The built-in protocol is experimental and does not match commercial drivers.

## CPU vectors

The CPU core is validated against the `SingleStepTests/65816` vectors. Download
the `v1/*.json` files yourself and point the runner at them:

```bash
cargo run --release -p emo-cli -- verify path/to/65816/v1
```

The runner executes one instruction per case on a flat 16 MB address space and
compares registers, RAM and the exact bus cycle count.

## Layout

| Path | Contents |
| --- | --- |
| `crates/emo-core` | Timing, address space, cartridge mapping, DMA/HDMA and the machine scheduler |
| `crates/emo-ppu` | Scanline PPU: registers, VRAM/CGRAM/OAM, backgrounds (0-4, 7), sprites, mosaic |
| `crates/emo-apu` | Audio subsystem: SPC700, IPL protocol, ARAM, ports, timers, S-DSP |
| `crates/emo-cpu` | 65C816 core: decoder, addressing modes, cycle accounting |
| `crates/emo-cli` | The `emo` binary: timing report and instruction vector runner |
| `docs/theory/` | Static theory guide on how the SNES works |
| `roms/` | Local ROM fixtures, never committed |

Further crates (CPU, PPU, APU, CLI, desktop frontend) are added as their
milestones start, so the repository never carries empty scaffolding.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
