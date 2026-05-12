//! Sidecar JSON that records what a local mp3 was uploaded as on LISTEN.
//!
//! Written next to the audio file as `<name>.mp3.json`. Co-locating with the
//! mp3 means deleting the recording naturally cleans up its metadata too —
//! no orphan-row reconciliation like a central index would need.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::upload::listen::EpisodeSummary;

/// What we know about an mp3 after a successful LISTEN upload.
///
/// `uploaded_at` is stored as RFC 3339 so the file is human-readable and
/// the wire format is decoupled from chrono types (easy to evolve later).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeMetadata {
    pub episode_id: String,
    pub title: String,
    pub webview_url: String,
    pub uploaded_at: String,
}

/// `recording_xxx.mp3` → `recording_xxx.mp3.json`.
///
/// `Path::with_extension("json")` would replace `.mp3`, not append, so we
/// build the sidecar name by appending to the OsString instead.
pub fn sidecar_path(mp3_path: &Path) -> PathBuf {
    let mut s: OsString = mp3_path.as_os_str().to_owned();
    s.push(".json");
    PathBuf::from(s)
}

pub fn write(mp3_path: &Path, meta: &EpisodeMetadata) -> Result<()> {
    let path = sidecar_path(mp3_path);
    let body = serde_json::to_vec_pretty(meta).context("serialize episode metadata")?;
    std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Delete the sidecar JSON next to `mp3_path`, if present. Idempotent —
/// a missing sidecar reports `Ok` because the postcondition (sidecar gone)
/// is already satisfied, and callers like `rehydrate` don't want to stat
/// first just to avoid a NotFound. The mp3 itself is never touched.
pub fn remove(mp3_path: &Path) -> Result<()> {
    let path = sidecar_path(mp3_path);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
    }
}

/// Returns `None` when the sidecar is missing or unreadable. Read errors
/// (corrupt JSON, permission denied) deliberately don't propagate — a single
/// bad sidecar should never blank the whole Recent panel.
pub fn read(mp3_path: &Path) -> Option<EpisodeMetadata> {
    let path = sidecar_path(mp3_path);
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Walk `dir` for `<name>.mp3.json` sidecars, parse them, and pair each with
/// the matching mp3 path. Sidecars whose JSON is corrupt or whose mp3 is
/// missing are silently dropped — a single bad row mustn't poison the whole
/// rehydrate pass. Returns `(mp3_path, parsed_metadata)` pairs.
pub fn collect_sidecars(dir: &Path) -> Vec<(PathBuf, EpisodeMetadata)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // We're looking specifically for `*.mp3.json` so plain `.json` files
        // a user might have left in the dir don't get parsed as metadata.
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.ends_with(".mp3.json") {
            continue;
        }
        let mp3_path = PathBuf::from(path.to_string_lossy().trim_end_matches(".json").to_string());
        if !mp3_path.exists() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<EpisodeMetadata>(&bytes) else {
            continue;
        };
        out.push((mp3_path, meta));
    }
    out
}

/// Apply a server-side episode summary onto a local sidecar in place. Returns
/// `true` when something actually changed so callers can avoid spurious
/// writes (and the noisy timestamps that come with them). `uploaded_at` is
/// preserved on purpose: it records when *this user* uploaded the file, not
/// when LISTEN last touched the episode.
pub fn apply_remote(meta: &mut EpisodeMetadata, summary: &EpisodeSummary) -> bool {
    let mut changed = false;
    if let Some(remote_title) = summary.title.as_deref()
        && remote_title != meta.title
    {
        meta.title = remote_title.to_string();
        changed = true;
    }
    if summary.webview_url != meta.webview_url {
        meta.webview_url = summary.webview_url.clone();
        changed = true;
    }
    changed
}

/// Build an `EpisodeMetadata` with `uploaded_at` stamped from the local
/// clock and write it next to `mp3_path`. Returns the value that was
/// written so the caller can hand it to the UI without re-reading.
pub fn record_upload(
    mp3_path: &Path,
    episode_id: &str,
    title: &str,
    webview_url: &str,
) -> Result<EpisodeMetadata> {
    let meta = EpisodeMetadata {
        episode_id: episode_id.to_string(),
        title: title.to_string(),
        webview_url: webview_url.to_string(),
        uploaded_at: chrono::Local::now().to_rfc3339(),
    };
    write(mp3_path, &meta)?;
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RAII temp dir for filesystem tests. Cleans up on `Drop` so a panicking
    /// assertion can't leak a dir under `/tmp` between runs. Pre-removes any
    /// stale dir from a previous failed run before recreating, since two
    /// concurrent test invocations sharing the same name would otherwise
    /// fight (we keep names per-test so this stays a non-issue in practice).
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

    fn fixture() -> EpisodeMetadata {
        EpisodeMetadata {
            episode_id: "ep_abc123".to_string(),
            title: "My great episode".to_string(),
            webview_url: "https://listen.style/p/foo/ep_abc123".to_string(),
            uploaded_at: "2026-05-01T12:34:56+09:00".to_string(),
        }
    }

    #[test]
    fn sidecar_path_appends_json_to_mp3() {
        let p = sidecar_path(Path::new("/tmp/recording_2026.mp3"));
        assert_eq!(p, PathBuf::from("/tmp/recording_2026.mp3.json"));
    }

    #[test]
    fn sidecar_path_preserves_extra_dots_in_name() {
        // Extension replacement would clobber `.foo`; we want pure append.
        let p = sidecar_path(Path::new("/tmp/r.foo.mp3"));
        assert_eq!(p, PathBuf::from("/tmp/r.foo.mp3.json"));
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = TestDir::new("radi_test_metadata_roundtrip");
        let mp3 = dir.path().join("recording.mp3");

        let meta = fixture();
        write(&mp3, &meta).unwrap();
        let loaded = read(&mp3).expect("sidecar should be readable");
        assert_eq!(loaded, meta);
    }

    #[test]
    fn remove_deletes_existing_sidecar() {
        let dir = TestDir::new("radi_test_metadata_remove_exists");
        let mp3 = dir.path().join("recording.mp3");
        std::fs::write(&mp3, b"\xFF\xFB").unwrap();
        write(&mp3, &fixture()).unwrap();
        assert!(sidecar_path(&mp3).exists());

        remove(&mp3).unwrap();
        assert!(!sidecar_path(&mp3).exists());
        // The mp3 must be left alone — only the upload claim is retracted.
        assert!(mp3.exists());
    }

    #[test]
    fn remove_is_ok_when_sidecar_already_gone() {
        // Idempotent: callers (e.g. rehydrate) shouldn't have to stat first.
        // A second call on the same path must not error.
        let dir = TestDir::new("radi_test_metadata_remove_missing");
        let mp3 = dir.path().join("recording.mp3");
        assert!(!sidecar_path(&mp3).exists());
        remove(&mp3).unwrap();
    }

    #[test]
    fn read_returns_none_when_sidecar_missing() {
        let mp3 = std::env::temp_dir().join("radi_test_metadata_missing.mp3");
        let _ = std::fs::remove_file(sidecar_path(&mp3));
        assert!(read(&mp3).is_none());
    }

    #[test]
    fn record_upload_writes_sidecar_with_rfc3339_timestamp() {
        let dir = TestDir::new("radi_test_metadata_record");
        let mp3 = dir.path().join("recording.mp3");

        let written = record_upload(
            &mp3,
            "ep_xyz",
            "Hello world",
            "https://listen.style/p/x/ep_xyz",
        )
        .unwrap();
        assert_eq!(written.episode_id, "ep_xyz");
        assert_eq!(written.title, "Hello world");
        assert_eq!(written.webview_url, "https://listen.style/p/x/ep_xyz");
        // Round-trippable through chrono so consumers (e.g. a future "show
        // upload age" feature) don't have to reparse a free-form string.
        chrono::DateTime::parse_from_rfc3339(&written.uploaded_at)
            .expect("uploaded_at must be RFC 3339");

        let on_disk = read(&mp3).expect("sidecar should exist after record_upload");
        assert_eq!(on_disk, written);
    }

    #[test]
    fn read_returns_none_on_corrupt_json() {
        // A single corrupt sidecar must not propagate up and break scanning.
        let dir = TestDir::new("radi_test_metadata_corrupt");
        let mp3 = dir.path().join("recording.mp3");
        std::fs::write(sidecar_path(&mp3), b"{not json").unwrap();
        assert!(read(&mp3).is_none());
    }

    #[test]
    fn collect_sidecars_returns_pairs_for_valid_entries() {
        let dir = TestDir::new("radi_test_metadata_collect_pairs");
        let mp3 = dir.path().join("recording.mp3");
        std::fs::write(&mp3, b"\xFF\xFB").unwrap(); // arbitrary mp3 bytes; existence is what matters
        let meta = fixture();
        write(&mp3, &meta).unwrap();

        let pairs = collect_sidecars(dir.path());
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, mp3);
        assert_eq!(pairs[0].1, meta);
    }

    #[test]
    fn collect_sidecars_skips_orphan_sidecar_with_no_mp3() {
        // mp3 was removed but its sidecar lingered: there's nothing to
        // attach the rehydrated title to, so dropping the entry is fine.
        let dir = TestDir::new("radi_test_metadata_collect_orphan");
        let mp3 = dir.path().join("recording.mp3");
        write(&mp3, &fixture()).unwrap();
        // Note: never created mp3 itself.
        assert!(collect_sidecars(dir.path()).is_empty());
    }

    #[test]
    fn collect_sidecars_skips_corrupt_sidecar() {
        let dir = TestDir::new("radi_test_metadata_collect_corrupt");
        let mp3 = dir.path().join("recording.mp3");
        std::fs::write(&mp3, b"\xFF\xFB").unwrap();
        std::fs::write(sidecar_path(&mp3), b"{not json").unwrap();
        // Corrupt sidecars must drop out cleanly so a single bad row can't
        // abort the whole rehydrate scan.
        assert!(collect_sidecars(dir.path()).is_empty());
    }

    #[test]
    fn apply_remote_updates_title_and_url_when_changed() {
        let mut meta = fixture();
        let summary = EpisodeSummary {
            id: meta.episode_id.clone(),
            title: Some("New title".into()),
            webview_url: "https://listen.style/p/foo/new".into(),
        };
        assert!(apply_remote(&mut meta, &summary));
        assert_eq!(meta.title, "New title");
        assert_eq!(meta.webview_url, "https://listen.style/p/foo/new");
    }

    #[test]
    fn apply_remote_preserves_uploaded_at() {
        // uploaded_at records when the local user uploaded the file. The
        // server's idea of "last touched" is irrelevant to that, so
        // rehydrate must never overwrite it.
        let mut meta = fixture();
        let original_uploaded_at = meta.uploaded_at.clone();
        let summary = EpisodeSummary {
            id: meta.episode_id.clone(),
            title: Some("Edited".into()),
            webview_url: meta.webview_url.clone(),
        };
        apply_remote(&mut meta, &summary);
        assert_eq!(meta.uploaded_at, original_uploaded_at);
    }

    #[test]
    fn apply_remote_returns_false_when_nothing_changed() {
        // No-op summary should report `false` so the caller can skip the
        // disk write — otherwise every startup rewrites every sidecar.
        let mut meta = fixture();
        let summary = EpisodeSummary {
            id: meta.episode_id.clone(),
            title: Some(meta.title.clone()),
            webview_url: meta.webview_url.clone(),
        };
        assert!(!apply_remote(&mut meta, &summary));
    }

    #[test]
    fn apply_remote_keeps_local_title_when_remote_title_is_null() {
        // A null `title` from the server (e.g. transient draft state)
        // should not blank out the local one.
        let mut meta = fixture();
        let original_title = meta.title.clone();
        let summary = EpisodeSummary {
            id: meta.episode_id.clone(),
            title: None,
            webview_url: meta.webview_url.clone(),
        };
        assert!(!apply_remote(&mut meta, &summary));
        assert_eq!(meta.title, original_title);
    }
}
