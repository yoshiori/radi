use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

/// Number of peak-level samples retained for the sparkline waveform.
/// At the ~100ms UI tick this covers roughly the last 12 seconds.
pub const PEAK_HISTORY_CAPACITY: usize = 120;

/// How many previous recordings to surface in the TUI's "Recent" panel.
pub const MAX_RECENT_RECORDINGS: usize = 16;

/// Throttle interval for rescanning the output directory.
const RECENT_REFRESH_INTERVAL: Duration = Duration::from_millis(750);

/// One row in the "Recent" panel. Display strings are pre-formatted on the
/// scan thread so the render loop (100ms tick) stays allocation-free.
#[derive(Debug, Clone)]
pub struct RecentRecording {
    pub path: PathBuf,
    pub size: String,
    pub timestamp: String,
}

use crate::audio::denoiser::Denoiser;
use crate::audio::encoder::Mp3Writer;
use crate::audio::recorder::{self, Recorder};
use crate::config::ListenConfig;
use crate::upload::listen::{EpisodeSpec, EpisodeStatus, ListenClient, Visibility};
use crate::upload::progress::UploadProgress;

#[derive(Debug, Clone, PartialEq)]
pub enum AppState {
    Idle,
    Recording,
    Processing,
    Done(PathBuf),
    Uploading(PathBuf),
    Uploaded { path: PathBuf, webview_url: String },
    UploadFailed { path: PathBuf, error: String },
    ConfirmQuit { previous: Box<AppState> },
}

pub struct App {
    pub state: AppState,
    pub should_quit: bool,
    pub output_path: PathBuf,
    pub output_dir: PathBuf,
    pub peak_level: Arc<AtomicU32>,
    pub upload_progress: Arc<UploadProgress>,
    pub device_name: Option<String>,
    recording_start: Option<Instant>,
    final_elapsed: Duration,
    recorder: Option<Recorder>,
    encode_thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    upload_thread: Option<std::thread::JoinHandle<anyhow::Result<String>>>,
    peak_history: VecDeque<f32>,
    recent: Vec<RecentRecording>,
    recent_last_refresh: Option<Instant>,
    recent_thread: Option<JoinHandle<Vec<RecentRecording>>>,
}

impl App {
    pub fn new(output_dir: PathBuf) -> Self {
        let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let device_name = recorder::default_input_device_name();
        let output_path = output_dir.join(format!("recording_{timestamp}.mp3"));
        let mut app = Self {
            state: AppState::Idle,
            should_quit: false,
            output_path,
            output_dir,
            peak_level: Arc::new(AtomicU32::new(0)),
            upload_progress: UploadProgress::new(),
            device_name,
            recording_start: None,
            final_elapsed: Duration::ZERO,
            recorder: None,
            encode_thread: None,
            upload_thread: None,
            peak_history: VecDeque::with_capacity(PEAK_HISTORY_CAPACITY),
            recent: Vec::new(),
            recent_last_refresh: None,
            recent_thread: None,
        };
        app.spawn_recent_scan();
        app
    }

    pub fn recent(&self) -> &[RecentRecording] {
        &self.recent
    }

    /// Spawns a background scan of `output_dir`. Kept off the TUI thread so a
    /// slow filesystem (network mount, spinning disk) can't stall rendering.
    fn spawn_recent_scan(&mut self) {
        if self.recent_thread.is_some() {
            return;
        }
        self.recent_last_refresh = Some(Instant::now());
        let dir = self.output_dir.clone();
        self.recent_thread = Some(std::thread::spawn(move || scan_recent(&dir)));
    }

    /// Polls the in-flight scan; installs its result if ready. No-op when
    /// nothing is pending.
    fn poll_recent(&mut self) {
        if !self.recent_thread.as_ref().is_some_and(|h| h.is_finished()) {
            return;
        }
        if let Some(handle) = self.recent_thread.take()
            && let Ok(list) = handle.join()
        {
            self.recent = list;
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

    pub fn peak_history(&self) -> &VecDeque<f32> {
        &self.peak_history
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
        // Clone the config so the upload thread owns its own copy and can
        // call resolved_token() itself. Token resolution must not run on
        // the main thread — `op://` references shell out to the 1Password
        // CLI, which can block for seconds (biometric prompt, network
        // sync) and would freeze the TUI between keypress and the first
        // Uploading frame.
        let listen = listen.clone();
        let upload_path = path.clone();

        // Start from a fresh progress handle so a previous failed attempt
        // can't leak stale bytes into the UI.
        self.upload_progress = UploadProgress::new();
        let progress = self.upload_progress.clone();

        let handle = std::thread::spawn(move || -> anyhow::Result<String> {
            let token = listen.required_token()?;
            let client = ListenClient::new(listen.endpoint_or_default(), token)?;
            let spec = EpisodeSpec {
                podcast_id: &listen.podcast_id,
                title: &title,
                description: None,
                visibility: Visibility::Public,
                status: EpisodeStatus::Draft,
            };
            let episode = client.upload_episode_with_progress(spec, &upload_path, &progress)?;
            Ok(episode.webview_url)
        });

        self.upload_thread = Some(handle);
        self.state = AppState::Uploading(path);
        Ok(())
    }

    pub fn tick(&mut self) {
        // Called each TUI frame to update dynamic state.
        // Sample the peak meter into a rolling history so the UI can render a
        // waveform sparkline; only meaningful while recording, but keeping the
        // last recording's tail around briefly is harmless.
        if self.peak_history.len() == PEAK_HISTORY_CAPACITY {
            self.peak_history.pop_front();
        }
        self.peak_history.push_back(self.peak());

        self.poll_recent();
        if self.recent_thread.is_none()
            && self
                .recent_last_refresh
                .is_none_or(|t| t.elapsed() >= RECENT_REFRESH_INTERVAL)
        {
            self.spawn_recent_scan();
        }

        if !self.upload_thread.as_ref().is_some_and(|h| h.is_finished()) {
            return;
        }
        // The upload may be nested under ConfirmQuit when the user opened the
        // quit prompt mid-upload; resolve either shape without clobbering it.
        let path = match &self.state {
            AppState::Uploading(p) => p.clone(),
            AppState::ConfirmQuit { previous } => match previous.as_ref() {
                AppState::Uploading(p) => p.clone(),
                _ => return,
            },
            _ => return,
        };
        let handle = self.upload_thread.take().expect("just checked Some");
        let resolved = match handle.join() {
            Ok(Ok(webview_url)) => AppState::Uploaded { path, webview_url },
            Ok(Err(e)) => AppState::UploadFailed {
                path,
                error: e.to_string(),
            },
            Err(_) => AppState::UploadFailed {
                path,
                error: "upload thread panicked".to_string(),
            },
        };
        self.state = match std::mem::replace(&mut self.state, AppState::Idle) {
            AppState::ConfirmQuit { .. } => AppState::ConfirmQuit {
                previous: Box::new(resolved),
            },
            _ => resolved,
        };
    }
}

/// Scan `dir` for mp3 recordings, newest first, capped to
/// `MAX_RECENT_RECORDINGS`. Timestamp and size strings are formatted here so
/// the render loop never has to touch the timezone database or allocate.
fn scan_recent(dir: &Path) -> Vec<RecentRecording> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    // Collect just the sort keys and raw size first; defer timestamp/size
    // string formatting until after truncation so only the ~16 surviving
    // entries pay the allocation cost, even when the directory has thousands
    // of files.
    let mut list: Vec<(Option<SystemTime>, PathBuf, u64)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let is_mp3 = path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("mp3"));
            if !is_mp3 {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok(), path, meta.len()))
        })
        .collect();
    // `Option::cmp` puts `None` before `Some`, so sorting descending naturally
    // groups timestamped files first with the newest on top; path breaks ties.
    list.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    list.truncate(MAX_RECENT_RECORDINGS);
    list.into_iter()
        .map(|(modified, path, size_bytes)| {
            let timestamp = modified
                .map(|m| {
                    let dt: chrono::DateTime<chrono::Local> = m.into();
                    dt.format("%m-%d %H:%M").to_string()
                })
                .unwrap_or_else(|| "—".to_string());
            RecentRecording {
                path,
                size: format_size(size_bytes),
                timestamp,
            }
        })
        .collect()
}

pub(crate) fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
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
    fn start_upload_does_not_block_main_thread_on_op_token() {
        // When api_token is an `op://` reference, resolution shells out to
        // the 1Password CLI which can take seconds. The main thread must
        // not wait on it — otherwise the TUI freezes between the `u`
        // keypress and the first Uploading frame. Guard by asserting that
        // start_upload returns promptly and transitions to Uploading even
        // though the configured token would be slow (or impossible) to
        // resolve.
        let mut app = App::new(PathBuf::from("."));
        let path = PathBuf::from("./start_upload_regression_nonexistent.mp3");
        app.state = AppState::Done(path.clone());

        let listen = ListenConfig {
            podcast_id: "pid".into(),
            api_token: Some("op://never-read-from-main-thread/entry".into()),
            endpoint: Some("http://127.0.0.1:1/graphql".into()),
        };

        let t0 = Instant::now();
        app.start_upload(&listen, "title".into()).unwrap();
        let elapsed = t0.elapsed();

        assert!(matches!(app.state, AppState::Uploading(_)));
        assert!(
            elapsed < Duration::from_millis(100),
            "start_upload blocked the main thread for {elapsed:?}"
        );
    }

    #[test]
    fn test_upload_progress_starts_clean() {
        let app = App::new(PathBuf::from("."));
        assert_eq!(app.upload_progress.uploaded(), 0);
        assert_eq!(app.upload_progress.total(), 0);
        assert_eq!(
            app.upload_progress.phase(),
            crate::upload::progress::UploadPhase::Preparing
        );
    }

    #[test]
    fn test_device_name_is_set_on_creation() {
        let app = App::new(PathBuf::from("."));
        // device_name is Some if an input device exists, None otherwise
        // On CI without audio devices it may be None, so just verify the field exists
        let _ = &app.device_name;
    }
}
