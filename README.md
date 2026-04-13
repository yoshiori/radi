# radi

A TUI podcast recorder written in Rust. Captures audio from your default input
device, applies ML-based noise suppression (RNNoise via `nnnoiseless`), and
encodes the result to MP3 in a streaming pipeline.

## Features

- Terminal UI built with [ratatui](https://ratatui.rs/) and crossterm
- Real-time peak level meter
- ML noise suppression (RNNoise / `nnnoiseless`)
- Streaming MP3 encoding (LAME, 48 kHz)
- One-key upload of finished recordings to [LISTEN](https://listen.style/) as a DRAFT episode
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

| Key | Action                                                      |
| --- | ----------------------------------------------------------- |
| `r` | Start a new recording (also: retake after a finished take)  |
| `s` | Stop and save the current take                              |
| `u` | Upload the finished take to LISTEN as a DRAFT episode       |
| `q` | Quit (stops the current take too)                           |

Recordings are saved as `recording_YYYY-MM-DD_HH-MM-SS.mp3`.

## Configuration

Create `~/.config/radi/config.toml` to customize the output directory and
LISTEN upload settings:

```toml
output_dir = "/home/you/Podcasts"

[listen]
podcast_id = "01xxxxxxxxxxxxxxxxxxxxxxxx"
api_token = "your-listen-api-token"
# endpoint = "https://listen.style/graphql"  # optional override
```

`api_token` can also be omitted and supplied via the `LISTEN_API_TOKEN`
environment variable. If the file is missing or `output_dir` is unset,
recordings are written to the current working directory. If the `[listen]`
section is absent, pressing `u` reports the missing configuration instead of
silently doing nothing.

### LISTEN upload

`u` uploads the finished MP3 via LISTEN's GraphQL API:

1. `createPresignedUploadUrl` → S3 presigned URL
2. HTTP `PUT` the file to that URL
3. `createEpisode` with the returned `path`, creating a **DRAFT** episode so
   you can review and publish it on the LISTEN web UI.

Get a token from the LISTEN web UI (user menu → **API Tokens**).

`api_token` values prefixed with `op://` are also supported and resolved at
runtime via the [1Password CLI](https://developer.1password.com/docs/cli/) if
you prefer not to store the secret in plain text.

## Architecture

`radi` runs three concurrent threads:

1. **Audio capture** — `cpal` callback delivers 44.1 kHz mono `f32` samples and
   updates the peak level via an atomic.
2. **Encoding** — receives samples, resamples to 48 kHz with `rubato`, denoises
   with `nnnoiseless` (480-sample frames), encodes to MP3 with `mp3lame`.
3. **Main / TUI** — polls keyboard events at 100 ms, redraws the UI, and drives
   the state machine `Idle → Recording → Processing → Done → Uploading →
   Uploaded / UploadFailed`.

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
