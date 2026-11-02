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
| `crates/emo-core` | Master-clock timing, bus and machine wiring |
| `crates/emo-cpu` | 65C816 core: decoder, addressing modes, cycle accounting |
| `crates/emo-cli` | The `emo` binary: timing report and instruction vector runner |
| `docs/theory/` | Static theory guide on how the SNES works |
| `roms/` | Local ROM fixtures, never committed |

Further crates (CPU, PPU, APU, CLI, desktop frontend) are added as their
milestones start, so the repository never carries empty scaffolding.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
