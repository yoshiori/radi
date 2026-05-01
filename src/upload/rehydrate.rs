//! Startup pass that re-syncs local sidecars against LISTEN.
//!
//! After uploading, a user usually goes to the LISTEN web UI and edits the
//! episode title (and sometimes the slug, which changes `webviewUrl`).
//! Without this pass, Recent panel rows would display the title we sent at
//! upload time forever — typically the timestamped filename. Running this
//! once at startup keeps the local view in sync with the latest server
//! state, at the cost of one batched GraphQL round-trip.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use crate::config::ListenConfig;
use crate::upload::listen::ListenClient;
use crate::upload::metadata;

/// Look at every sidecar under `output_dir`, ask LISTEN for the current
/// episode summary for each, and overwrite local titles / URLs that drifted.
/// Returns the number of sidecars that actually changed on disk so the
/// caller can log a meaningful "synced N" — entries that are already
/// up-to-date don't trigger a write.
pub fn rehydrate(output_dir: &Path, listen: &ListenConfig) -> Result<usize> {
    let pairs = metadata::collect_sidecars(output_dir);
    if pairs.is_empty() {
        return Ok(0);
    }

    let token = listen.required_token()?;
    let client = ListenClient::new(listen.endpoint_or_default(), token)?;
    let ids: Vec<String> = pairs.iter().map(|(_, m)| m.episode_id.clone()).collect();
    let summaries = client.fetch_episodes(&listen.podcast_id, &ids)?;

    // Index server-side summaries so each local sidecar can find its match
    // in O(1). LISTEN may return fewer rows than we asked for (deleted /
    // not-visible-to-token episodes); those drop out of the map and the
    // corresponding local sidecars are left untouched on purpose.
    let by_id: HashMap<&str, &_> = summaries.iter().map(|s| (s.id.as_str(), s)).collect();

    let mut updated = 0;
    for (path, mut meta) in pairs {
        if let Some(summary) = by_id.get(meta.episode_id.as_str())
            && metadata::apply_remote(&mut meta, summary)
        {
            // Best-effort: a single failed write shouldn't abort the
            // remaining rows. The next startup will retry anyway.
            if metadata::write(&path, &meta).is_ok() {
                updated += 1;
            }
        }
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upload::metadata::{EpisodeMetadata, write};

    /// RAII temp dir for filesystem tests. Cleans up on `Drop` so a panicking
    /// assertion can't leak a dir under `/tmp` between runs.
    struct TestDir(std::path::PathBuf);

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

    fn seed(dir: &Path, name: &str, episode_id: &str, title: &str) -> std::path::PathBuf {
        let mp3 = dir.join(name);
        std::fs::write(&mp3, b"\xFF\xFB").unwrap();
        write(
            &mp3,
            &EpisodeMetadata {
                episode_id: episode_id.to_string(),
                title: title.to_string(),
                webview_url: format!("https://listen.style/old/{episode_id}"),
                uploaded_at: "2026-04-01T00:00:00+00:00".to_string(),
            },
        )
        .unwrap();
        mp3
    }

    #[test]
    fn rehydrate_updates_drifted_titles_and_returns_change_count() {
        let dir = TestDir::new("radi_test_rehydrate_updates");
        let mp3a = seed(dir.path(), "a.mp3", "ep_a", "old A");
        let mp3b = seed(dir.path(), "b.mp3", "ep_b", "B unchanged");

        let mut server = mockito::Server::new();
        // Both episodes are in the PUBLISHED bucket, so the early-return in
        // fetch_episodes resolves them in one round-trip.
        let mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(
                r#"{"data":{"podcast":{"episodes":{
                    "paginatorInfo":{"hasMorePages":false},
                    "data":[
                        {"id":"ep_a","title":"new A","webviewUrl":"https://listen.style/new/ep_a"},
                        {"id":"ep_b","title":"B unchanged","webviewUrl":"https://listen.style/old/ep_b"}
                    ]
                }}}}"#,
            )
            .create();

        let listen = ListenConfig {
            podcast_id: "pod".into(),
            api_token: Some("t".into()),
            endpoint: Some(format!("{}/graphql", server.url())),
        };

        let updated = rehydrate(dir.path(), &listen).unwrap();
        // Only ep_a drifted; ep_b matched server-side and shouldn't trigger
        // a redundant write.
        assert_eq!(updated, 1);
        mock.assert();

        let after_a = metadata::read(&mp3a).unwrap();
        assert_eq!(after_a.title, "new A");
        assert_eq!(after_a.webview_url, "https://listen.style/new/ep_a");
        assert_eq!(
            after_a.uploaded_at, "2026-04-01T00:00:00+00:00",
            "uploaded_at must be preserved across rehydrate"
        );

        let after_b = metadata::read(&mp3b).unwrap();
        assert_eq!(after_b.title, "B unchanged");
    }

    #[test]
    fn rehydrate_with_no_sidecars_skips_network() {
        // If the dir has no sidecars there's nothing to refresh, so we
        // shouldn't even hit the network. mockito with no registered mocks
        // would 501 any request, which makes this assertion implicit.
        let dir = TestDir::new("radi_test_rehydrate_empty");
        let server = mockito::Server::new();
        let listen = ListenConfig {
            podcast_id: "pod".into(),
            api_token: Some("t".into()),
            endpoint: Some(format!("{}/graphql", server.url())),
        };
        let updated = rehydrate(dir.path(), &listen).unwrap();
        assert_eq!(updated, 0);
    }

    #[test]
    fn rehydrate_leaves_sidecar_untouched_when_episode_missing_server_side() {
        // Episode deleted on LISTEN → every status bucket comes back empty
        // → fetch_episodes returns no summary for it → keep the local
        // sidecar so the user still sees the title they had.
        let dir = TestDir::new("radi_test_rehydrate_missing_server");
        let mp3 = seed(dir.path(), "gone.mp3", "ep_gone", "still here");

        let mut server = mockito::Server::new();
        // One mock matching all three status passes; expect_at_least(1)
        // tolerates the per-status loop without pinning to an exact count.
        let _m = server
            .mock("POST", "/graphql")
            .expect_at_least(1)
            .with_status(200)
            .with_body(
                r#"{"data":{"podcast":{"episodes":{
                    "paginatorInfo":{"hasMorePages":false},
                    "data":[]
                }}}}"#,
            )
            .create();

        let listen = ListenConfig {
            podcast_id: "pod".into(),
            api_token: Some("t".into()),
            endpoint: Some(format!("{}/graphql", server.url())),
        };

        let updated = rehydrate(dir.path(), &listen).unwrap();
        assert_eq!(updated, 0);
        let after = metadata::read(&mp3).unwrap();
        assert_eq!(after.title, "still here");
    }
}
