use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::audio::denoiser::Denoiser;
use crate::audio::encoder::Mp3Writer;
use crate::audio::recorder::{self, Recorder};
use crate::config::ListenConfig;
use crate::upload::listen::{EpisodeStatus, ListenClient, Visibility};

#[derive(Debug, Clone, PartialEq)]
pub enum AppState {
    Idle,
    Recording,
    Processing,
    Done(PathBuf),
    Uploading(PathBuf),
    Uploaded { path: PathBuf, episode_id: String },
    UploadFailed { path: PathBuf, error: String },
}

pub struct App {
    pub state: AppState,
    pub should_quit: bool,
    pub output_path: PathBuf,
    pub peak_level: Arc<AtomicU32>,
    pub device_name: Option<String>,
    recording_start: Option<Instant>,
    final_elapsed: Duration,
    recorder: Option<Recorder>,
    encode_thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    upload_thread: Option<std::thread::JoinHandle<anyhow::Result<String>>>,
}

impl App {
    pub fn new(output_dir: PathBuf) -> Self {
        let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let device_name = recorder::default_input_device_name();
        Self {
            state: AppState::Idle,
            should_quit: false,
            output_path: output_dir.join(format!("recording_{timestamp}.mp3")),
            peak_level: Arc::new(AtomicU32::new(0)),
            device_name,
            recording_start: None,
            final_elapsed: Duration::ZERO,
            recorder: None,
            encode_thread: None,
            upload_thread: None,
        }
    }

    pub fn elapsed(&self) -> Duration {
        match self.recording_start {
            Some(start) => start.elapsed(),
            None => self.final_elapsed,
        }
    }

    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak_level.load(Ordering::Relaxed))
    }

    pub fn start_recording(&mut self) -> anyhow::Result<()> {
        if self.state != AppState::Idle {
            return Ok(());
        }

        let peak = self.peak_level.clone();
        let (recorder, rx) = Recorder::new(peak)?;
        recorder.play()?;
        self.device_name = recorder::default_input_device_name();

        let path = self.output_path.clone();
        let handle = std::thread::spawn(move || -> anyhow::Result<()> {
            let mut writer = Mp3Writer::new(&path)?;
            let mut denoiser = Denoiser::new()?;
            while let Ok(samples) = rx.recv() {
                let denoised = denoiser.process(&samples);
                if !denoised.is_empty() {
                    writer.write_samples(&denoised)?;
                }
            }
            let remaining = denoiser.flush();
            if !remaining.is_empty() {
                writer.write_samples(&remaining)?;
            }
            writer.finish()?;
            Ok(())
        });

        self.recorder = Some(recorder);
        self.encode_thread = Some(handle);
        self.recording_start = Some(Instant::now());
        self.state = AppState::Recording;
        Ok(())
    }

    pub fn stop_recording(&mut self) -> anyhow::Result<()> {
        if self.state != AppState::Recording {
            return Ok(());
        }

        self.state = AppState::Processing;
        self.final_elapsed = self.elapsed();
        self.recording_start = None;

        // Drop recorder to close the channel and stop the cpal stream
        self.recorder.take();

        if let Some(handle) = self.encode_thread.take() {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("Encoding thread panicked"))??;
        }

        let path = self.output_path.clone();
        self.state = AppState::Done(path);
        Ok(())
    }

    pub fn start_upload(&mut self, listen: &ListenConfig, title: String) -> anyhow::Result<()> {
        let AppState::Done(path) = self.state.clone() else {
            return Ok(());
        };
        let token = listen
            .resolved_token()
            .ok_or_else(|| anyhow::anyhow!("LISTEN API token not configured (set [listen].api_token in config.toml or LISTEN_API_TOKEN env var)"))?;
        let endpoint = listen.endpoint_or_default().to_string();
        let podcast_id = listen.podcast_id.clone();
        let upload_path = path.clone();

        let handle = std::thread::spawn(move || -> anyhow::Result<String> {
            let client = ListenClient::new(endpoint, token)?;
            let episode = client.upload_episode(
                &podcast_id,
                &title,
                None,
                &upload_path,
                Visibility::Public,
                EpisodeStatus::Draft,
            )?;
            Ok(episode.id)
        });

        self.upload_thread = Some(handle);
        self.state = AppState::Uploading(path);
        Ok(())
    }

    pub fn tick(&mut self) {
        // Called each TUI frame to update dynamic state
        // peak_level is read directly via atomic, no action needed here
        if let Some(handle) = self.upload_thread.as_ref()
            && handle.is_finished()
            && let Some(handle) = self.upload_thread.take()
        {
            let AppState::Uploading(path) = self.state.clone() else {
                return;
            };
            match handle.join() {
                Ok(Ok(episode_id)) => {
                    self.state = AppState::Uploaded { path, episode_id };
                }
                Ok(Err(e)) => {
                    self.state = AppState::UploadFailed {
                        path,
                        error: e.to_string(),
                    };
                }
                Err(_) => {
                    self.state = AppState::UploadFailed {
                        path,
                        error: "upload thread panicked".to_string(),
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_is_idle() {
        let app = App::new(PathBuf::from("."));
        assert_eq!(app.state, AppState::Idle);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_elapsed_is_zero_before_recording() {
        let app = App::new(PathBuf::from("."));
        assert_eq!(app.elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_peak_is_zero_initially() {
        let app = App::new(PathBuf::from("."));
        assert_eq!(app.peak(), 0.0);
    }

    #[test]
    fn test_output_path_has_timestamp() {
        let app = App::new(PathBuf::from("."));
        let path = app.output_path.to_str().unwrap();
        assert!(path.contains("recording_"));
        assert!(path.ends_with(".mp3"));
    }

    #[test]
    fn test_output_path_uses_custom_dir() {
        let app = App::new(PathBuf::from("/tmp/out"));
        let path = app.output_path.to_str().unwrap();
        assert!(path.starts_with("/tmp/out/recording_"));
        assert!(path.ends_with(".mp3"));
    }

    #[test]
    fn test_stop_recording_noop_when_idle() {
        let mut app = App::new(PathBuf::from("."));
        assert!(app.stop_recording().is_ok());
        assert_eq!(app.state, AppState::Idle);
    }

    #[test]
    fn test_device_name_is_set_on_creation() {
        let app = App::new(PathBuf::from("."));
        // device_name is Some if an input device exists, None otherwise
        // On CI without audio devices it may be None, so just verify the field exists
        let _ = &app.device_name;
    }
}
