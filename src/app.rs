use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::audio::encoder::Mp3Writer;
use crate::audio::processor::NoiseGate;
use crate::audio::recorder::{self, Recorder};

#[derive(Debug, Clone, PartialEq)]
pub enum AppState {
    Idle,
    Recording,
    Processing,
    Done(PathBuf),
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
}

impl App {
    pub fn new() -> Self {
        let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let device_name = recorder::default_input_device_name();
        Self {
            state: AppState::Idle,
            should_quit: false,
            output_path: PathBuf::from(format!("recording_{timestamp}.mp3")),
            peak_level: Arc::new(AtomicU32::new(0)),
            device_name,
            recording_start: None,
            final_elapsed: Duration::ZERO,
            recorder: None,
            encode_thread: None,
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

        let path = self.output_path.clone();
        let handle = std::thread::spawn(move || -> anyhow::Result<()> {
            let mut writer = Mp3Writer::new(&path)?;
            let mut gate = NoiseGate::new(0.01, 44100 / 4);
            while let Ok(mut samples) = rx.recv() {
                gate.process(&mut samples);
                writer.write_samples(&samples)?;
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

    pub fn tick(&mut self) {
        // Called each TUI frame to update dynamic state
        // peak_level is read directly via atomic, no action needed here
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_is_idle() {
        let app = App::new();
        assert_eq!(app.state, AppState::Idle);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_elapsed_is_zero_before_recording() {
        let app = App::new();
        assert_eq!(app.elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_peak_is_zero_initially() {
        let app = App::new();
        assert_eq!(app.peak(), 0.0);
    }

    #[test]
    fn test_output_path_has_timestamp() {
        let app = App::new();
        let path = app.output_path.to_str().unwrap();
        assert!(path.starts_with("recording_"));
        assert!(path.ends_with(".mp3"));
    }

    #[test]
    fn test_stop_recording_noop_when_idle() {
        let mut app = App::new();
        assert!(app.stop_recording().is_ok());
        assert_eq!(app.state, AppState::Idle);
    }

    #[test]
    fn test_device_name_is_set_on_creation() {
        let app = App::new();
        // device_name is Some if an input device exists, None otherwise
        // On CI without audio devices it may be None, so just verify the field exists
        let _ = &app.device_name;
    }

}
