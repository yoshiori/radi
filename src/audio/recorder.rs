use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};

pub struct Recorder {
    stream: cpal::Stream,
}

impl Recorder {
    pub fn new(peak: Arc<AtomicU32>) -> anyhow::Result<(Self, mpsc::Receiver<Vec<f32>>)> {
        let host = cpal::default_host();

        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No input device found"))?;

        let config = StreamConfig {
            channels: 1,
            sample_rate: SampleRate(44_100),
            buffer_size: cpal::BufferSize::Default,
        };

        let (tx, rx) = mpsc::channel::<Vec<f32>>();

        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                let peak_val = data.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                peak.store(peak_val.to_bits(), Ordering::Relaxed);
                let _ = tx.send(data.to_vec());
            },
            |err| eprintln!("Audio stream error: {err}"),
            None,
        )?;

        Ok((Self { stream }, rx))
    }

    pub fn play(&self) -> anyhow::Result<()> {
        self.stream.play()?;
        Ok(())
    }
}

/// Query the default audio source name from WirePlumber/PipeWire.
/// Falls back to cpal device name if wpctl is unavailable.
pub fn default_input_device_name() -> Option<String> {
    if let Some(name) = wpctl_default_source_name() {
        return Some(name);
    }
    // Fallback: cpal device name
    cpal::default_host()
        .default_input_device()
        .and_then(|d| d.name().ok())
}

fn wpctl_default_source_name() -> Option<String> {
    let output = Command::new("wpctl")
        .args(["inspect", "@DEFAULT_AUDIO_SOURCE@"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_wpctl_description(&String::from_utf8_lossy(&output.stdout))
}

fn parse_wpctl_description(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        // wpctl prefixes "currently active" fields with "* ", e.g.
        //   "  * node.description = \"…\""
        // so strip leading whitespace plus an optional "* " marker.
        let trimmed = line.trim_start().trim_start_matches("* ");
        if let Some(value) = trimmed.strip_prefix("node.description = ") {
            return Some(value.trim_matches('"').to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wpctl_description() {
        let input = "id 54, type PipeWire:Interface:Node\n  node.description = \"Built-in Audio Analog Stereo\"\n  alsa.card_name = \"Built-in Audio\"";
        assert_eq!(
            parse_wpctl_description(input),
            Some("Built-in Audio Analog Stereo".to_string())
        );
    }

    #[test]
    fn test_parse_wpctl_description_with_active_marker() {
        let input = "id 92, type PipeWire:Interface:Node\n  * node.description = \"ATR2100x-USB Microphone\"\n  * node.nick = \"ATR2100x\"";
        assert_eq!(
            parse_wpctl_description(input),
            Some("ATR2100x-USB Microphone".to_string())
        );
    }

    #[test]
    fn test_parse_wpctl_description_missing() {
        let input = "id 54, type PipeWire:Interface:Node\n  alsa.card_name = \"Built-in Audio\"";
        assert_eq!(parse_wpctl_description(input), None);
    }
}
