use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, StreamConfig};

pub struct Recorder {
    stream: cpal::Stream,
}

impl Recorder {
    pub fn new(
        peak: Arc<AtomicU32>,
    ) -> anyhow::Result<(Self, mpsc::Receiver<Vec<f32>>)> {
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
