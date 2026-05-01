//! Sidecar JSON that records what a local mp3 was uploaded as on LISTEN.
//!
//! Written next to the audio file as `<name>.mp3.json`. Co-locating with the
//! mp3 means deleting the recording naturally cleans up its metadata too —
//! no orphan-row reconciliation like a central index would need.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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

/// Returns `None` when the sidecar is missing or unreadable. Read errors
/// (corrupt JSON, permission denied) deliberately don't propagate — a single
/// bad sidecar should never blank the whole Recent panel.
pub fn read(mp3_path: &Path) -> Option<EpisodeMetadata> {
    let path = sidecar_path(mp3_path);
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
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
        let dir = std::env::temp_dir().join("radi_test_metadata_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mp3 = dir.join("recording.mp3");

        let meta = fixture();
        write(&mp3, &meta).unwrap();
        let loaded = read(&mp3).expect("sidecar should be readable");
        assert_eq!(loaded, meta);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_returns_none_when_sidecar_missing() {
        let mp3 = std::env::temp_dir().join("radi_test_metadata_missing.mp3");
        let _ = std::fs::remove_file(sidecar_path(&mp3));
        assert!(read(&mp3).is_none());
    }

    #[test]
    fn record_upload_writes_sidecar_with_rfc3339_timestamp() {
        let dir = std::env::temp_dir().join("radi_test_metadata_record");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mp3 = dir.join("recording.mp3");

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

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_returns_none_on_corrupt_json() {
        // A single corrupt sidecar must not propagate up and break scanning.
        let dir = std::env::temp_dir().join("radi_test_metadata_corrupt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mp3 = dir.join("recording.mp3");
        std::fs::write(sidecar_path(&mp3), b"{not json").unwrap();
        assert!(read(&mp3).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
