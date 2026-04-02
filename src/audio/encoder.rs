use std::fs::File;
use std::io::Write;
use std::path::Path;

use mp3lame_encoder::{Builder, Encoder, FlushNoGap, MonoPcm};

pub struct Mp3Writer {
    encoder: Encoder,
    file: File,
    mp3_buffer: Vec<u8>,
}

impl Mp3Writer {
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        let mut builder = Builder::new().ok_or_else(|| anyhow::anyhow!("Failed to create LAME builder"))?;
        builder
            .set_num_channels(1)
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        builder
            .set_sample_rate(44_100)
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        builder
            .set_quality(mp3lame_encoder::Quality::Best)
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        let encoder = builder.build().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let file = File::create(path)?;

        Ok(Self {
            encoder,
            file,
            mp3_buffer: Vec::new(),
        })
    }

    pub fn write_samples(&mut self, pcm_f32: &[f32]) -> anyhow::Result<()> {
        let pcm_i16: Vec<i16> = pcm_f32
            .iter()
            .map(|&s| {
                let clamped = s.clamp(-1.0, 1.0);
                (clamped * i16::MAX as f32) as i16
            })
            .collect();

        let input = MonoPcm(&pcm_i16);
        let required = mp3lame_encoder::max_required_buffer_size(pcm_i16.len());
        self.mp3_buffer.reserve(required);

        let encoded_size = self
            .encoder
            .encode(input, self.mp3_buffer.spare_capacity_mut())
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        unsafe {
            self.mp3_buffer.set_len(encoded_size);
        }
        self.file.write_all(&self.mp3_buffer)?;
        self.mp3_buffer.clear();
        Ok(())
    }

    pub fn finish(&mut self) -> anyhow::Result<()> {
        let required = mp3lame_encoder::max_required_buffer_size(0);
        self.mp3_buffer.reserve(required);

        let flushed = self
            .encoder
            .flush::<FlushNoGap>(self.mp3_buffer.spare_capacity_mut())
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        unsafe {
            self.mp3_buffer.set_len(flushed);
        }
        self.file.write_all(&self.mp3_buffer)?;
        self.file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_encode_silence_produces_mp3() {
        let path = PathBuf::from("/tmp/radi_test_silence.mp3");
        let mut writer = Mp3Writer::new(&path).unwrap();

        let silence = vec![0.0f32; 44100];
        writer.write_samples(&silence).unwrap();
        writer.finish().unwrap();

        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.len() > 0, "MP3 file should not be empty");

        let data = std::fs::read(&path).unwrap();
        let has_mp3_sync = data
            .windows(2)
            .any(|w| w[0] == 0xFF && (w[1] & 0xE0) == 0xE0);
        assert!(has_mp3_sync, "File should contain MP3 sync bytes");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_encode_tone_produces_mp3() {
        let path = PathBuf::from("/tmp/radi_test_tone.mp3");
        let mut writer = Mp3Writer::new(&path).unwrap();

        let samples: Vec<f32> = (0..44100)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44100.0).sin() * 0.5)
            .collect();
        writer.write_samples(&samples).unwrap();
        writer.finish().unwrap();

        let metadata = std::fs::metadata(&path).unwrap();
        assert!(
            metadata.len() > 100,
            "MP3 with audio content should be substantial"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_f32_to_i16_clipping() {
        let path = PathBuf::from("/tmp/radi_test_clip.mp3");
        let mut writer = Mp3Writer::new(&path).unwrap();

        let samples = vec![2.0f32, -2.0, 0.0, 1.0, -1.0];
        writer.write_samples(&samples).unwrap();
        writer.finish().unwrap();

        std::fs::remove_file(&path).ok();
    }
}
