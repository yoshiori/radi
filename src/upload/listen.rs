//! LISTEN (listen.style) episode upload client.
//!
//! Flow (see docs discovered via GraphQL introspection):
//!   1. `createPresignedUploadUrl` returns `{uploadUrl, path, publicUrl}`
//!   2. HTTP PUT the audio bytes to `uploadUrl`
//!   3. `createEpisode` with `audioPath = path` creates the episode
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use serde_json::{Value, json};

use crate::upload::progress::{ProgressReader, UploadPhase, UploadProgress};

const CREATE_PRESIGNED_MUTATION: &str = r#"
mutation($fileName: String!, $type: MediaType!, $contentType: String) {
  createPresignedUploadUrl(fileName: $fileName, type: $type, contentType: $contentType) {
    uploadUrl
    path
    publicUrl
  }
}
"#;

const CREATE_EPISODE_MUTATION: &str = r#"
mutation($podcastId: ID!, $title: String!, $description: String,
         $visibility: EpisodeVisibilityType!, $audioPath: String!,
         $status: EpisodeStatus) {
  createEpisode(podcastId: $podcastId, title: $title, description: $description,
                visibility: $visibility, audioPath: $audioPath, status: $status) {
    id
    webviewUrl
  }
}
"#;

#[derive(Debug, Clone, Copy)]
pub enum Visibility {
    Public,
}

impl Visibility {
    fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "PUBLIC",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EpisodeStatus {
    Draft,
}

impl EpisodeStatus {
    fn as_str(self) -> &'static str {
        match self {
            EpisodeStatus::Draft => "DRAFT",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUpload {
    pub upload_url: String,
    pub path: String,
    pub public_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedEpisode {
    pub id: String,
    pub webview_url: String,
}

/// Fixed metadata for a new episode — bundled so the upload entry point
/// stays under the clippy `too_many_arguments` lint.
#[derive(Debug, Clone, Copy)]
pub struct EpisodeSpec<'a> {
    pub podcast_id: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub visibility: Visibility,
    pub status: EpisodeStatus,
}

pub struct ListenClient {
    endpoint: String,
    token: String,
    client: reqwest::blocking::Client,
}

impl ListenClient {
    pub fn new(endpoint: impl Into<String>, token: impl Into<String>) -> anyhow::Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .context("build reqwest client")?;
        Ok(Self {
            endpoint: endpoint.into(),
            token: token.into(),
            client,
        })
    }

    fn graphql(&self, query: &str, variables: Value) -> anyhow::Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .context("send graphql request")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("graphql http {}: {}", status, body));
        }
        let json: Value = resp.json().context("decode graphql response")?;
        if let Some(errors) = json.get("errors").filter(|e| !is_empty_errors(e)) {
            return Err(anyhow!("graphql errors: {errors}"));
        }
        json.get("data")
            .cloned()
            .ok_or_else(|| anyhow!("graphql response missing data"))
    }

    pub fn create_presigned_upload(
        &self,
        file_name: &str,
        content_type: &str,
    ) -> anyhow::Result<PresignedUpload> {
        let data = self.graphql(
            CREATE_PRESIGNED_MUTATION,
            json!({
                "fileName": file_name,
                "type": "AUDIO",
                "contentType": content_type,
            }),
        )?;
        let node = data
            .get("createPresignedUploadUrl")
            .ok_or_else(|| anyhow!("response missing createPresignedUploadUrl"))?;
        Ok(PresignedUpload {
            upload_url: string_field(node, "uploadUrl")?,
            path: string_field(node, "path")?,
            public_url: string_field(node, "publicUrl")?,
        })
    }

    /// Streams `file_path` to `upload_url` with a PUT and publishes byte
    /// progress into `progress`. Sets `total` from the file size before
    /// streaming, then `ProgressReader` increments `uploaded` as reqwest
    /// pulls chunks. `Body::sized` is used explicitly because S3 presigned
    /// URLs require `Content-Length`.
    pub fn put_audio_with_progress(
        &self,
        upload_url: &str,
        file_path: &Path,
        content_type: &str,
        progress: &Arc<UploadProgress>,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::open(file_path)
            .with_context(|| format!("open audio file {}", file_path.display()))?;
        let len = file
            .metadata()
            .with_context(|| format!("stat audio file {}", file_path.display()))?
            .len();
        progress.set_total(len);

        let reader = ProgressReader::new(file, progress.clone());
        let body = reqwest::blocking::Body::sized(reader, len);
        let resp = self
            .client
            .put(upload_url)
            .header("Content-Type", content_type)
            .body(body)
            .send()
            .context("send presigned PUT")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(anyhow!("S3 PUT failed {}: {}", status, text));
        }
        Ok(())
    }

    pub fn create_episode(
        &self,
        podcast_id: &str,
        title: &str,
        description: Option<&str>,
        audio_path: &str,
        visibility: Visibility,
        status: EpisodeStatus,
    ) -> anyhow::Result<UploadedEpisode> {
        let data = self.graphql(
            CREATE_EPISODE_MUTATION,
            json!({
                "podcastId": podcast_id,
                "title": title,
                "description": description,
                "visibility": visibility.as_str(),
                "audioPath": audio_path,
                "status": status.as_str(),
            }),
        )?;
        let node = data
            .get("createEpisode")
            .ok_or_else(|| anyhow!("response missing createEpisode"))?;
        Ok(UploadedEpisode {
            id: string_field(node, "id")?,
            webview_url: string_field(node, "webviewUrl")?,
        })
    }

    /// End-to-end: presign → PUT → createEpisode. Publishes phase and byte
    /// progress into `progress` so a UI can show a live gauge.
    pub fn upload_episode_with_progress(
        &self,
        spec: EpisodeSpec<'_>,
        mp3_path: &Path,
        progress: &Arc<UploadProgress>,
    ) -> anyhow::Result<UploadedEpisode> {
        let file_name = mp3_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid mp3 filename"))?;
        let content_type = "audio/mpeg";

        progress.set_phase(UploadPhase::Preparing);
        let presigned = self.create_presigned_upload(file_name, content_type)?;

        progress.set_phase(UploadPhase::Uploading);
        self.put_audio_with_progress(&presigned.upload_url, mp3_path, content_type, progress)?;

        progress.set_phase(UploadPhase::Finalizing);
        self.create_episode(
            spec.podcast_id,
            spec.title,
            spec.description,
            &presigned.path,
            spec.visibility,
            spec.status,
        )
    }
}

/// Treat an absent/null/empty-array `errors` value as "no errors"; any
/// non-empty or non-array payload is surfaced to the caller.
fn is_empty_errors(errors: &Value) -> bool {
    errors.is_null() || errors.as_array().is_some_and(|a| a.is_empty())
}

fn string_field(value: &Value, key: &str) -> anyhow::Result<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("missing string field {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ListenConfig;

    #[test]
    fn visibility_enum_serializes_as_expected() {
        assert_eq!(Visibility::Public.as_str(), "PUBLIC");
    }

    #[test]
    fn episode_status_enum_serializes_as_expected() {
        assert_eq!(EpisodeStatus::Draft.as_str(), "DRAFT");
    }

    #[test]
    fn create_presigned_parses_response() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/graphql")
            .match_header("authorization", "Bearer t")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"createPresignedUploadUrl":{
                    "uploadUrl":"https://s3.example/put?sig=1",
                    "path":"uploads/2026/abc.mp3",
                    "publicUrl":"https://cdn.example/abc.mp3"
                }}}"#,
            )
            .create();

        let client = ListenClient::new(format!("{}/graphql", server.url()), "t").unwrap();
        let result = client
            .create_presigned_upload("foo.mp3", "audio/mpeg")
            .unwrap();

        assert_eq!(result.upload_url, "https://s3.example/put?sig=1");
        assert_eq!(result.path, "uploads/2026/abc.mp3");
        mock.assert();
    }

    #[test]
    fn graphql_errors_are_surfaced() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"errors":[{"message":"forbidden"}]}"#)
            .create();

        let client = ListenClient::new(format!("{}/graphql", server.url()), "t").unwrap();
        let err = client
            .create_presigned_upload("foo.mp3", "audio/mpeg")
            .unwrap_err();
        assert!(err.to_string().contains("forbidden"), "got: {err}");
    }

    #[test]
    fn put_audio_with_progress_reports_total_and_uploaded_bytes() {
        let mut server = mockito::Server::new();
        let tmp = std::env::temp_dir().join("radi-test-put-progress.mp3");
        let payload = b"hello-mp3-payload"; // 17 bytes
        std::fs::write(&tmp, payload).unwrap();

        let mock = server
            .mock("PUT", "/put-progress")
            .match_header("content-length", "17")
            .match_header("content-type", "audio/mpeg")
            .match_body("hello-mp3-payload")
            .with_status(200)
            .create();

        let progress = UploadProgress::new();
        let client = ListenClient::new(format!("{}/graphql", server.url()), "t").unwrap();
        client
            .put_audio_with_progress(
                &format!("{}/put-progress", server.url()),
                &tmp,
                "audio/mpeg",
                &progress,
            )
            .unwrap();

        mock.assert();
        assert_eq!(progress.total(), 17);
        assert_eq!(progress.uploaded(), 17);
        let _ = std::fs::remove_file(&tmp);
    }

    /// Real upload against listen.style. Creates a DRAFT PRIVATE episode.
    ///
    /// Required env vars (otherwise the test is skipped):
    ///   LISTEN_API_TOKEN          — API bearer token
    ///   RADI_TEST_PODCAST_ID      — target podcast id
    ///   RADI_TEST_MP3             — absolute path to a small .mp3 to upload
    ///
    /// Run with: `cargo test --ignored upload_episode_to_listen_real`
    #[test]
    #[ignore = "hits the real listen.style API; opt-in via --ignored"]
    fn upload_episode_to_listen_real() {
        let (Ok(token), Ok(podcast_id), Ok(mp3_path)) = (
            std::env::var("LISTEN_API_TOKEN"),
            std::env::var("RADI_TEST_PODCAST_ID"),
            std::env::var("RADI_TEST_MP3"),
        ) else {
            eprintln!("skipping: set LISTEN_API_TOKEN, RADI_TEST_PODCAST_ID, RADI_TEST_MP3");
            return;
        };

        let client = ListenClient::new(ListenConfig::DEFAULT_ENDPOINT, token).unwrap();
        let path = std::path::PathBuf::from(&mp3_path);
        let title = format!(
            "radi-e2e-test {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        let spec = EpisodeSpec {
            podcast_id: &podcast_id,
            title: &title,
            description: Some("automated e2e test from radi"),
            visibility: Visibility::Public,
            status: EpisodeStatus::Draft,
        };
        let episode = client
            .upload_episode_with_progress(spec, &path, &UploadProgress::new())
            .expect("upload_episode succeeds");

        eprintln!("created episode url: {}", episode.webview_url);
        assert!(!episode.id.is_empty());
        assert!(!episode.webview_url.is_empty());
    }

    #[test]
    fn graphql_errors_non_array_is_surfaced() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(r#"{"errors":{"message":"boom"}}"#)
            .create();

        let client = ListenClient::new(format!("{}/graphql", server.url()), "t").unwrap();
        let err = client
            .create_presigned_upload("foo.mp3", "audio/mpeg")
            .unwrap_err();
        assert!(err.to_string().contains("boom"), "got: {err}");
    }

    #[test]
    fn graphql_empty_errors_array_is_ignored() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_body(
                r#"{"errors":[],"data":{"createPresignedUploadUrl":{"uploadUrl":"u","path":"p","publicUrl":"pu"}}}"#,
            )
            .create();

        let client = ListenClient::new(format!("{}/graphql", server.url()), "t").unwrap();
        client
            .create_presigned_upload("foo.mp3", "audio/mpeg")
            .expect("empty errors array should not fail");
    }

    #[test]
    fn create_episode_sends_variables() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::PartialJsonString(
                r#"{"variables":{"podcastId":"pod","title":"t","audioPath":"uploads/x.mp3","visibility":"PUBLIC","status":"DRAFT"}}"#
                    .into(),
            ))
            .with_status(200)
            .with_body(
                r#"{"data":{"createEpisode":{"id":"ep123","webviewUrl":"https://listen.style/p/pod/ep123"}}}"#,
            )
            .create();

        let client = ListenClient::new(format!("{}/graphql", server.url()), "t").unwrap();
        let ep = client
            .create_episode(
                "pod",
                "t",
                None,
                "uploads/x.mp3",
                Visibility::Public,
                EpisodeStatus::Draft,
            )
            .unwrap();
        assert_eq!(ep.id, "ep123");
        mock.assert();
    }
}
