# radi

A TUI podcast recorder written in Rust. Captures audio from your default input
device, applies ML-based noise suppression (RNNoise via `nnnoiseless`), and
encodes the result to MP3 in a streaming pipeline.

## Features

- Terminal UI built with [ratatui](https://ratatui.rs/) and crossterm
- Real-time peak level meter
- ML noise suppression (RNNoise / `nnnoiseless`)
- Streaming MP3 encoding (LAME, 48 kHz)
- Configurable output directory via `~/.config/radi/config.toml`
- Single binary, no daemons

## Requirements

- Rust (edition 2024)
- Linux: `libasound2-dev` and `libmp3lame-dev`
  ```bash
  sudo apt install libasound2-dev libmp3lame-dev
  ```
- macOS: `lame` (e.g. via `brew install lame`)

## Install

Install the latest version directly from the repository:

```bash
cargo install --git https://github.com/yoshiori/radi
```

Or from a local checkout:

```bash
cargo install --path .
```

This places the `radi` binary in `~/.cargo/bin/`, so make sure that directory
is on your `PATH`.

## Build from source

If you'd rather not install, you can just build it:

```bash
cargo build --release
./target/release/radi
```

## Usage

```bash
radi
```

### Key bindings

| Key | Action                            |
| --- | --------------------------------- |
| `R` | Start a new recording             |
| `S` | Stop and save the current take    |
| `Q` | Quit (stops the current take too) |

Recordings are saved as `recording_YYYY-MM-DD_HH-MM-SS.mp3`.

## Configuration

Create `~/.config/radi/config.toml` to customize the output directory:

```toml
output_dir = "/home/you/Podcasts"
```

If the file is missing or `output_dir` is unset, recordings are written to the
current working directory.

## Architecture

`radi` runs three concurrent threads:

1. **Audio capture** — `cpal` callback delivers 44.1 kHz mono `f32` samples and
   updates the peak level via an atomic.
2. **Encoding** — receives samples, resamples to 48 kHz with `rubato`, denoises
   with `nnnoiseless` (480-sample frames), encodes to MP3 with `mp3lame`.
3. **Main / TUI** — polls keyboard events at 100 ms, redraws the UI, and drives
   the state machine `Idle → Recording → Processing → Done`.

See `CLAUDE.md` for a more detailed module-level overview.

## Development

```bash
cargo test                    # run all tests
cargo clippy -- -D warnings   # lint
cargo fmt                     # format
```

This project follows TDD: tests first, then implementation, then refactoring.

## License

MIT License. See [LICENSE](LICENSE) for details.
