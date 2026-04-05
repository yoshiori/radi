use nnnoiseless::DenoiseState;
use rubato::{FastFixedIn, PolynomialDegree, Resampler};

const INPUT_SAMPLE_RATE: f64 = 44_100.0;
const OUTPUT_SAMPLE_RATE: f64 = 48_000.0;
const RESAMPLE_CHUNK_SIZE: usize = 1024;

pub struct Denoiser {
    denoise: Box<DenoiseState<'static>>,
    resampler: FastFixedIn<f32>,
    frame_buf: Vec<f32>,
}

impl Denoiser {
    pub fn new() -> anyhow::Result<Self> {
        let resampler = FastFixedIn::new(
            OUTPUT_SAMPLE_RATE / INPUT_SAMPLE_RATE,
            1.0,
            PolynomialDegree::Cubic,
            RESAMPLE_CHUNK_SIZE,
            1,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create resampler: {e}"))?;

        Ok(Self {
            denoise: DenoiseState::new(),
            resampler,
            frame_buf: Vec::new(),
        })
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        let resampled = self.resample(input);
        self.frame_buf.extend_from_slice(&resampled);
        self.process_frames()
    }

    pub fn flush(&mut self) -> Vec<f32> {
        // Zero-pad the remaining buffer to a full frame
        let remaining = self.frame_buf.len() % DenoiseState::FRAME_SIZE;
        if remaining > 0 {
            let padding = DenoiseState::FRAME_SIZE - remaining;
            self.frame_buf.extend(std::iter::repeat_n(0.0, padding));
        }
        self.process_frames()
    }

    fn resample(&mut self, input: &[f32]) -> Vec<f32> {
        let mut output = Vec::new();
        let mut pos = 0;

        while pos < input.len() {
            let chunk_len = (input.len() - pos).min(RESAMPLE_CHUNK_SIZE);
            let chunk = &input[pos..pos + chunk_len];
            pos += chunk_len;

            let waves_in = vec![chunk.to_vec()];
            match self.resampler.process(&waves_in, None) {
                Ok(waves_out) => {
                    if let Some(channel) = waves_out.first() {
                        output.extend_from_slice(channel);
                    }
                }
                Err(_) => continue,
            }
        }

        output
    }

    fn process_frames(&mut self) -> Vec<f32> {
        let frame_size = DenoiseState::FRAME_SIZE;
        let num_frames = self.frame_buf.len() / frame_size;
        let mut output = Vec::with_capacity(num_frames * frame_size);
        let mut out_frame = vec![0.0f32; frame_size];

        for i in 0..num_frames {
            let start = i * frame_size;
            let in_frame: Vec<f32> = self.frame_buf[start..start + frame_size]
                .iter()
                .map(|&s| s * 32767.0)
                .collect();

            self.denoise.process_frame(&mut out_frame, &in_frame);

            output.extend(out_frame.iter().map(|&s| s / 32767.0));
        }

        let consumed = num_frames * frame_size;
        self.frame_buf.drain(..consumed);

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_empty_input() {
        let mut denoiser = Denoiser::new().unwrap();
        let result = denoiser.process(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_process_silence() {
        let mut denoiser = Denoiser::new().unwrap();
        let silence = vec![0.0f32; 44100];
        let result = denoiser.process(&silence);
        // Output should be near-zero for silent input
        assert!(result.iter().all(|&s| s.abs() < 0.01));
    }

    #[test]
    fn test_output_length_ratio() {
        let mut denoiser = Denoiser::new().unwrap();
        let input_len = 44100;
        let input = vec![0.0f32; input_len];
        let mut output = denoiser.process(&input);
        output.extend(denoiser.flush());

        // Output should be approximately input_len * (48000/44100)
        let expected = (input_len as f64 * (OUTPUT_SAMPLE_RATE / INPUT_SAMPLE_RATE)) as usize;
        let tolerance = DenoiseState::FRAME_SIZE * 2;
        assert!(
            output.len().abs_diff(expected) < tolerance,
            "output len {} should be close to expected {}",
            output.len(),
            expected
        );
    }

    #[test]
    fn test_flush_processes_remaining() {
        let mut denoiser = Denoiser::new().unwrap();
        // Feed enough samples to produce some output, plus a partial frame remainder
        let input = vec![0.0f32; 4096];
        let result = denoiser.process(&input);
        let flushed = denoiser.flush();
        let total = result.len() + flushed.len();
        assert!(total > 0, "flush should produce output");
        // Flush should produce at least one frame from the remainder
        assert!(flushed.len() > 0, "flush should process remaining buffer");
    }
}
