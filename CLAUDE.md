# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Radi is a TUI podcast recorder written in Rust (edition 2024). It captures audio input, applies noise gate processing, and encodes output as MP3 files using a streaming pipeline.

## Build & Test Commands

```bash
cargo build                  # Build (requires libasound2-dev, libmp3lame-dev on Linux)
cargo test                   # Run all tests
cargo test <test_name>       # Run a single test
cargo clippy -- -D warnings  # Lint (treats warnings as errors)
cargo fmt                    # Format code
cargo fmt --check            # Check formatting without modifying
```

A postWrite hook automatically runs `cargo fmt` and `cargo clippy` on `.rs` files.

## Architecture

The app follows a state machine pattern with three concurrent threads:

1. **Audio capture thread** (cpal callback) → sends f32 samples via mpsc channel, updates peak level via `Arc<AtomicU32>`
2. **Encoding thread** → receives samples, applies noise gate (`processor.rs`), encodes to MP3 (`encoder.rs`), writes to file
3. **Main thread** → runs TUI event loop (100ms poll), reads peak level atomically, manages state transitions

### State Machine (`app.rs`)

`Idle` → `Recording` → `Processing` → `Done(PathBuf)`

- `start_recording()`: creates Recorder + spawns encoding thread
- `stop_recording()`: drops Recorder (stops audio stream) + joins encoding thread

### Module Layout

- `src/main.rs` — Entry point, keyboard handling, main event loop
- `src/app.rs` — App state machine, coordinates recorder and encoder
- `src/audio/recorder.rs` — cpal input stream capture (44.1kHz mono)
- `src/audio/encoder.rs` — MP3 encoding via libmp3lame (f32→i16 conversion)
- `src/audio/processor.rs` — Noise gate with amplitude threshold + hold time
- `src/tui/event.rs` — Crossterm keyboard event polling
- `src/tui/ui.rs` — Ratatui layout rendering (status, level meter, hints)
