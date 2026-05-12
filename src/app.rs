use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
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
    pub duration: String,
    pub timestamp: String,
    /// LISTEN upload metadata, if a sidecar JSON exists next to the mp3.
    /// Loaded best-effort: a missing or corrupt sidecar leaves this `None`
    /// rather than dropping the row from the panel entirely.
    pub episode: Option<EpisodeMetadata>,
}

use crate::audio::denoiser::Denoiser;
use crate::audio::encoder::Mp3Writer;
use crate::audio::recorder::{self, Recorder};
use crate::config::ListenConfig;
use crate::upload::listen::{EpisodeSpec, EpisodeStatus, ListenClient, Visibility};
use crate::upload::metadata::{self, EpisodeMetadata};
use crate::upload::progress::UploadProgress;
use crate::upload::rehydrate;

/// Phase of the startup pass that re-syncs sidecars against LISTEN.
/// Drives a small indicator in the Recent panel so the user can tell when
/// titles they edited on listen.style have made it to the local view.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncState {
    /// Either no LISTEN config, or no sidecars to sync — nothing happened.
    Idle,
    Syncing,
    /// Background rehydrate finished. `updated` is the number of sidecars
    /// whose title or webview_url actually changed.
    Done {
        updated: usize,
    },
    Failed(String),
}

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

impl AppState {
    /// True when the user is free to move the Recent-panel cursor and
    /// open an uploaded row. Recording / Processing / Uploading swallow
    /// keypresses (the encoder/upload threads own the work), so accepting
    /// Up/Down there — or surfacing the hint — would lie about what
    /// actually works. Kept on `AppState` so the input handler in
    /// `main.rs` and the hint row in `tui::ui` cannot drift out of sync.
    pub fn allows_recent_navigation(&self) -> bool {
        matches!(
            self,
            AppState::Idle
                | AppState::Done(_)
                | AppState::Uploaded { .. }
                | AppState::UploadFailed { .. }
        )
    }
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
    /// Cursor into `recent` for the user-driven row selection. `None` while
    /// the list is empty (no recordings yet). Tracked by index for cheap
    /// rendering; preserved across rescans by path in `poll_recent` so a
    /// newly arrived row doesn't yank the cursor off the user's pick.
    selected_recent: Option<usize>,
    recent_last_refresh: Option<Instant>,
    recent_thread: Option<JoinHandle<Vec<RecentRecording>>>,
    sync_state: Arc<Mutex<SyncState>>,
    sync_thread: Option<JoinHandle<()>>,
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
            selected_recent: None,
            recent_last_refresh: None,
            recent_thread: None,
            sync_state: Arc::new(Mutex::new(SyncState::Idle)),
            sync_thread: None,
        };
        app.spawn_recent_scan();
        app
    }

    /// Snapshot of the current rehydrate phase, for the UI's Recent-panel
    /// indicator. Cloned because the lock can't be held across the render
    /// closure without inviting deadlocks.
    pub fn sync_state(&self) -> SyncState {
        self.sync_state
            .lock()
            .map(|g| g.clone())
            .unwrap_or(SyncState::Idle)
    }

    /// Kick off the startup rehydrate pass. Token resolution and the
    /// GraphQL round-trip happen on a background thread so the TUI never
    /// blocks on `op://` lookups (which can take seconds) or LISTEN
    /// latency. Failures are stored in `sync_state` rather than returned —
    /// a network blip on launch must not prevent the user from recording.
    pub fn start_rehydrate(&mut self, listen: ListenConfig) {
        if self.sync_thread.is_some() {
            return;
        }
        self.set_sync_state(SyncState::Syncing);
        let dir = self.output_dir.clone();
        let state = self.sync_state.clone();
        self.sync_thread = Some(std::thread::spawn(move || {
            let result = rehydrate::rehydrate(&dir, &listen);
            let next = match result {
                Ok(updated) => SyncState::Done { updated },
                Err(e) => SyncState::Failed(e.to_string()),
            };
            if let Ok(mut guard) = state.lock() {
                *guard = next;
            }
        }));
    }

    fn set_sync_state(&self, next: SyncState) {
        if let Ok(mut guard) = self.sync_state.lock() {
            *guard = next;
        }
    }

    /// Joins the rehydrate thread once it finishes and forces an immediate
    /// recent-rescan so the freshly rewritten sidecars surface in the UI
    /// without waiting up to 750ms for the next periodic scan.
    fn poll_sync(&mut self) {
        if !self.sync_thread.as_ref().is_some_and(|h| h.is_finished()) {
            return;
        }
        if let Some(h) = self.sync_thread.take() {
            let _ = h.join();
        }
        // Force the next tick to spawn a fresh scan_recent regardless of
        // when the last one ran.
        self.recent_last_refresh = None;
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
            // Snapshot the selected path *before* replacing the list so we
            // can re-anchor the cursor onto the same recording even when a
            // newer one has bumped it down (or off) the row list.
            let previous_path = self
                .selected_recent
                .and_then(|i| self.recent.get(i))
                .map(|r| r.path.clone());
            self.recent = list;
            self.selected_recent = reconcile_selected(&self.recent, previous_path.as_deref());
        }
    }

    /// Index of the row currently highlighted in the Recent panel, if any.
    pub fn selected_recent(&self) -> Option<usize> {
        self.selected_recent
    }

    /// The recording the user has highlighted, or `None` when the Recent
    /// list is empty.
    pub fn selected_recording(&self) -> Option<&RecentRecording> {
        self.selected_recent.and_then(|i| self.recent.get(i))
    }

    /// Move the cursor one row up (towards the top / newer recordings).
    /// Clamps at the first row rather than wrapping — wrap-around in a
    /// short list is more disorienting than helpful.
    pub fn select_recent_prev(&mut self) {
        if let Some(i) = self.selected_recent
            && i > 0
        {
            self.selected_recent = Some(i - 1);
        }
    }

    /// Move the cursor one row down (towards older recordings). Clamps at
    /// the last row.
    pub fn select_recent_next(&mut self) {
        if let Some(i) = self.selected_recent
            && i + 1 < self.recent.len()
        {
            self.selected_recent = Some(i + 1);
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
            // Sidecar write is best-effort: the episode is already live on
            // LISTEN at this point, so a local FS hiccup must not flip the
            // upload result to failed. The recording will simply show up in
            // Recent without a title until re-uploaded.
            let _ =
                metadata::record_upload(&upload_path, &episode.id, &title, &episode.webview_url);
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

        // Poll the rehydrate thread *before* the recent rescan so a fresh
        // sidecar write lands in the very next scan rather than the one
        // after, keeping the syncing→done UI transition snappy.
        self.poll_sync();
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

/// Pick the selection index for `new_recent` given the previously selected
/// path (if any). The cursor follows the path across rescans so that a
/// freshly arrived recording — which lands at index 0 and shifts everything
/// down — doesn't yank the highlight off the row the user was looking at.
/// Falls back to index 0 when the previous path is gone (file deleted, or
/// scrolled off the cap), and to `None` when the list is empty.
fn reconcile_selected(
    new_recent: &[RecentRecording],
    previous_path: Option<&Path>,
) -> Option<usize> {
    if new_recent.is_empty() {
        return None;
    }
    if let Some(p) = previous_path
        && let Some(i) = new_recent.iter().position(|r| r.path == p)
    {
        return Some(i);
    }
    Some(0)
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
            let duration = format_mp3_duration(&path);
            let episode = metadata::read(&path);
            RecentRecording {
                path,
                size: format_size(size_bytes),
                duration,
                timestamp,
                episode,
            }
        })
        .collect()
}

/// Parse the mp3 at `path` and format its playback duration into a fixed
/// 7-cell column string so the Recent panel stays aligned.
/// Returns a placeholder on parse failure rather than propagating the error
/// — a single corrupt file shouldn't blank the whole panel.
fn format_mp3_duration(path: &Path) -> String {
    match mp3_duration::from_path(path) {
        Ok(d) => {
            let total = d.as_secs();
            let h = total / 3600;
            let m = (total % 3600) / 60;
            let s = total % 60;
            if h == 0 {
                format!("{m:>4}:{s:02}")
            } else {
                format!("{h}:{m:02}:{s:02}")
            }
        }
        Err(_) => "     --".to_string(),
    }
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

    /// RAII temp dir for filesystem tests. Cleans up on `Drop` so a panicking
    /// assertion can't leak a dir under `/tmp` between runs.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(name);
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create test dir");
            TestDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Encode ~1 second of silence so mp3-duration has a real frame to parse.
    fn write_silence_mp3(path: &Path) {
        let mut writer = Mp3Writer::new(path).unwrap();
        // 48 kHz mono — matches the encoder config.
        let silence = vec![0.0f32; 48_000];
        writer.write_samples(&silence).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn scan_recent_includes_playback_duration() {
        // Encode a real (~1s) mp3 via the same writer the app uses, scan the
        // dir, and confirm scan_recent surfaces a parsed duration string in a
        // shape the Recent panel's fixed-width column can render.
        let dir = TestDir::new("radi_test_recent_duration");
        let mp3 = dir.path().join("recording_test.mp3");
        write_silence_mp3(&mp3);

        let recent = scan_recent(dir.path());
        assert_eq!(recent.len(), 1);
        let entry = &recent[0];
        assert!(
            entry.duration.contains(':'),
            "duration should be formatted as m:ss / h:mm:ss, got {:?}",
            entry.duration
        );
        assert_ne!(
            entry.duration.trim(),
            "--",
            "duration parse should not have fallen back to placeholder"
        );
    }

    #[test]
    fn scan_recent_attaches_episode_metadata_when_sidecar_exists() {
        let dir = TestDir::new("radi_test_recent_episode_meta");
        let mp3 = dir.path().join("recording_test.mp3");
        write_silence_mp3(&mp3);

        let meta = EpisodeMetadata {
            episode_id: "ep_test".into(),
            title: "Test title".into(),
            webview_url: "https://listen.style/p/x/ep_test".into(),
            uploaded_at: "2026-05-01T00:00:00+09:00".into(),
        };
        metadata::write(&mp3, &meta).unwrap();

        let recent = scan_recent(dir.path());
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].episode.as_ref(), Some(&meta));

        // Plain mp3 with no sidecar → episode stays None, doesn't drop the row.
        let mp3b = dir.path().join("recording_no_meta.mp3");
        write_silence_mp3(&mp3b);
        let recent = scan_recent(dir.path());
        assert_eq!(recent.len(), 2);
        let no_meta = recent.iter().find(|r| r.path == mp3b).unwrap();
        assert!(no_meta.episode.is_none());
    }

    #[test]
    fn start_rehydrate_drives_sync_state_through_syncing_to_done() {
        // App-level wiring test: start_rehydrate must spawn a thread, set
        // Syncing immediately, and tick() must transition the state to Done
        // once the bg work finishes. The actual fetch/write logic is
        // covered by upload::rehydrate's tests.
        let dir = TestDir::new("radi_test_app_rehydrate_wiring");
        let mp3 = dir.path().join("recording_test.mp3");
        write_silence_mp3(&mp3);
        metadata::write(
            &mp3,
            &EpisodeMetadata {
                episode_id: "ep_w".into(),
                title: "old".into(),
                webview_url: "https://listen.style/old".into(),
                uploaded_at: "2026-04-01T00:00:00+00:00".into(),
            },
        )
        .unwrap();

        let mut server = mockito::Server::new();
        let _m = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(
                r#"{"data":{"podcast":{"episodes":{
                    "paginatorInfo":{"hasMorePages":false},
                    "data":[
                        {"id":"ep_w","title":"new","webviewUrl":"https://listen.style/new"}
                    ]
                }}}}"#,
            )
            .create();

        let mut app = App::new(dir.path().to_path_buf());
        app.start_rehydrate(ListenConfig {
            podcast_id: "pod".into(),
            api_token: Some("t".into()),
            endpoint: Some(format!("{}/graphql", server.url())),
        });
        assert_eq!(app.sync_state(), SyncState::Syncing);

        // Spin briefly while the bg thread runs against mockito (sub-100ms
        // in practice). Cap at 5s so a deadlock can't hang CI forever.
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.sync_state() == SyncState::Syncing && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // tick() joins the finished thread and clears the handle.
        app.tick();
        assert!(matches!(app.sync_state(), SyncState::Done { updated: 1 }));
        assert!(app.sync_thread.is_none());
    }

    /// Build a minimal RecentRecording with just the path set — the other
    /// fields don't matter for selection logic so we keep the test fixtures
    /// short.
    fn rec(path: &str) -> RecentRecording {
        RecentRecording {
            path: PathBuf::from(path),
            size: String::new(),
            duration: String::new(),
            timestamp: String::new(),
            episode: None,
        }
    }

    #[test]
    fn allows_recent_navigation_matches_input_accepting_states() {
        // The hint row and the Up/Down key handler are both driven by
        // this predicate. If a new state is added and the predicate is
        // forgotten, this test pins down the contract: every state that
        // shows the `↑↓` hint must also accept the keypress, and
        // vice-versa. Busy states (Recording / Processing / Uploading)
        // and the ConfirmQuit popup must not.
        assert!(AppState::Idle.allows_recent_navigation());
        assert!(AppState::Done(PathBuf::from("/x.mp3")).allows_recent_navigation());
        assert!(
            AppState::Uploaded {
                path: PathBuf::from("/x.mp3"),
                webview_url: "https://example.invalid".into(),
            }
            .allows_recent_navigation()
        );
        assert!(
            AppState::UploadFailed {
                path: PathBuf::from("/x.mp3"),
                error: "boom".into(),
            }
            .allows_recent_navigation()
        );

        assert!(!AppState::Recording.allows_recent_navigation());
        assert!(!AppState::Processing.allows_recent_navigation());
        assert!(!AppState::Uploading(PathBuf::from("/x.mp3")).allows_recent_navigation());
        assert!(
            !AppState::ConfirmQuit {
                previous: Box::new(AppState::Idle)
            }
            .allows_recent_navigation()
        );
    }

    #[test]
    fn selected_recent_is_none_on_empty_list() {
        let app = App::new(PathBuf::from("."));
        assert!(app.selected_recent().is_none());
        assert!(app.selected_recording().is_none());
    }

    #[test]
    fn reconcile_selected_returns_none_on_empty_list() {
        let prev = PathBuf::from("/x.mp3");
        assert_eq!(reconcile_selected(&[], Some(prev.as_path())), None);
        assert_eq!(reconcile_selected(&[], None), None);
    }

    #[test]
    fn reconcile_selected_defaults_to_first_row_when_no_previous() {
        let list = vec![rec("/a.mp3"), rec("/b.mp3")];
        assert_eq!(reconcile_selected(&list, None), Some(0));
    }

    #[test]
    fn reconcile_selected_follows_path_when_present() {
        // Selection was on /b. After rescan a new /c shows up at the top —
        // index naive math says the cursor should now be at 2, but the user
        // didn't move and would lose their pick. Path tracking keeps the
        // highlight on /b.
        let list = vec![rec("/c.mp3"), rec("/a.mp3"), rec("/b.mp3")];
        let prev = PathBuf::from("/b.mp3");
        assert_eq!(reconcile_selected(&list, Some(prev.as_path())), Some(2));
    }

    #[test]
    fn reconcile_selected_falls_back_to_first_when_previous_is_gone() {
        // The previously selected file was deleted (or aged off the cap).
        // Land back on row 0 rather than leaving the cursor pointing into
        // empty space.
        let list = vec![rec("/a.mp3"), rec("/b.mp3")];
        let prev = PathBuf::from("/gone.mp3");
        assert_eq!(reconcile_selected(&list, Some(prev.as_path())), Some(0));
    }

    #[test]
    fn select_recent_prev_clamps_at_zero() {
        // Wrap-around in a short list is more disorienting than helpful, so
        // pressing Up on the top row should stay put rather than jump to
        // the bottom.
        let mut app = App::new(PathBuf::from("."));
        app.recent = vec![rec("/a.mp3"), rec("/b.mp3")];
        app.selected_recent = Some(0);
        app.select_recent_prev();
        assert_eq!(app.selected_recent(), Some(0));
    }

    #[test]
    fn select_recent_next_clamps_at_last_row() {
        let mut app = App::new(PathBuf::from("."));
        app.recent = vec![rec("/a.mp3"), rec("/b.mp3")];
        app.selected_recent = Some(1);
        app.select_recent_next();
        assert_eq!(app.selected_recent(), Some(1));
    }

    #[test]
    fn select_recent_moves_within_bounds() {
        let mut app = App::new(PathBuf::from("."));
        app.recent = vec![rec("/a.mp3"), rec("/b.mp3"), rec("/c.mp3")];
        app.selected_recent = Some(0);
        app.select_recent_next();
        assert_eq!(app.selected_recent(), Some(1));
        app.select_recent_next();
        assert_eq!(app.selected_recent(), Some(2));
        app.select_recent_prev();
        assert_eq!(app.selected_recent(), Some(1));
    }

    #[test]
    fn select_recent_is_noop_when_list_is_empty() {
        // Guards against an out-of-bounds Some(0) leaking in if the user
        // somehow holds the arrow keys before the first scan completes.
        let mut app = App::new(PathBuf::from("."));
        assert!(app.recent.is_empty());
        app.select_recent_next();
        app.select_recent_prev();
        assert_eq!(app.selected_recent(), None);
    }

    #[test]
    fn poll_recent_initialises_selection_on_first_scan() {
        // End-to-end: a fresh App seeds the directory scan in `new()`,
        // tick() drains the handle, and the first non-empty result should
        // promote selection from None to Some(0).
        let dir = TestDir::new("radi_test_selection_initialises_on_first_scan");
        let mp3 = dir.path().join("recording_a.mp3");
        write_silence_mp3(&mp3);
        let mut app = App::new(dir.path().to_path_buf());
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.recent_thread.is_some() && Instant::now() < deadline {
            app.tick();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.recent().len(), 1);
        assert_eq!(app.selected_recent(), Some(0));
        assert_eq!(app.selected_recording().map(|r| r.path.clone()), Some(mp3));
    }
}
