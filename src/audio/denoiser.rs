use nnnoiseless::DenoiseState;
use rubato::{FastFixedIn, PolynomialDegree, Resampler};

const INPUT_SAMPLE_RATE: f64 = 44_100.0;
const OUTPUT_SAMPLE_RATE: f64 = 48_000.0;
const RESAMPLE_CHUNK_SIZE: usize = 1024;

pub struct Denoiser {
    denoise: Box<DenoiseState<'static>>,
    resampler: FastFixedIn<f32>,
    resample_buf: Vec<f32>,
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
            resample_buf: Vec::with_capacity(RESAMPLE_CHUNK_SIZE),
            frame_buf: Vec::new(),
        })
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.resample_buf.extend_from_slice(input);
        let resampled = self.resample();
        self.frame_buf.extend_from_slice(&resampled);
        self.process_frames()
    }

    pub fn flush(&mut self) -> Vec<f32> {
        // Flush remaining samples in resample buffer
        if !self.resample_buf.is_empty() {
            self.resample_buf
                .resize(RESAMPLE_CHUNK_SIZE, 0.0);
            let resampled = self.resample();
            self.frame_buf.extend_from_slice(&resampled);
        }

        // Zero-pad the remaining frame buffer to a full frame
        let remaining = self.frame_buf.len() % DenoiseState::FRAME_SIZE;
        if remaining > 0 {
            let new_len = self.frame_buf.len() + DenoiseState::FRAME_SIZE - remaining;
            self.frame_buf.resize(new_len, 0.0);
        }
        self.process_frames()
    }

    fn resample(&mut self) -> Vec<f32> {
        let mut output = Vec::new();
        let mut pos = 0;

        while pos + RESAMPLE_CHUNK_SIZE <= self.resample_buf.len() {
            let chunk = &self.resample_buf[pos..pos + RESAMPLE_CHUNK_SIZE];
            pos += RESAMPLE_CHUNK_SIZE;

            let waves_in = [chunk];
            match self.resampler.process(&waves_in, None) {
                Ok(waves_out) => {
                    if let Some(channel) = waves_out.first() {
                        output.extend_from_slice(channel);
                    }
                }
                Err(e) => {
                    eprintln!("Resampler error: {e}");
                }
            }
        }

        self.resample_buf.drain(..pos);
        output
    }

    fn process_frames(&mut self) -> Vec<f32> {
        let frame_size = DenoiseState::FRAME_SIZE;
        let num_frames = self.frame_buf.len() / frame_size;
        let mut output = Vec::with_capacity(num_frames * frame_size);
        let mut in_frame = [0.0f32; DenoiseState::FRAME_SIZE];
        let mut out_frame = [0.0f32; DenoiseState::FRAME_SIZE];

        for i in 0..num_frames {
            let start = i * frame_size;
            for (dst, &src) in in_frame
                .iter_mut()
                .zip(self.frame_buf[start..start + frame_size].iter())
            {
                *dst = src * 32767.0;
            }

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
        // Tolerance accounts for resample chunk padding on flush + frame padding
        let tolerance = RESAMPLE_CHUNK_SIZE * 2;
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
        assert!(!flushed.is_empty(), "flush should process remaining buffer");
    }

    #[test]
    fn test_small_input_buffered() {
        let mut denoiser = Denoiser::new().unwrap();
        // Feed less than one resample chunk — should buffer without losing samples
        let input = vec![0.1f32; 200];
        let result1 = denoiser.process(&input);
        // Feed more to complete a chunk
        let input2 = vec![0.1f32; 900];
        let result2 = denoiser.process(&input2);
        let flushed = denoiser.flush();
        let total = result1.len() + result2.len() + flushed.len();
        assert!(total > 0, "buffered small inputs should produce output");
    }
}
