use std::path::PathBuf;

#[derive(Debug, Default, serde::Deserialize, PartialEq)]
pub struct Config {
    pub output_dir: Option<PathBuf>,
    pub listen: Option<ListenConfig>,
}

#[derive(Debug, serde::Deserialize, PartialEq)]
pub struct ListenConfig {
    pub podcast_id: String,
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
}

impl ListenConfig {
    pub const DEFAULT_ENDPOINT: &'static str = "https://listen.style/graphql";

    /// Token from config, falling back to the LISTEN_API_TOKEN env var.
    ///
    /// Values prefixed with `op://` are resolved via the 1Password CLI
    /// (`op read <ref>`), so config.toml can reference a vault item without
    /// storing the secret on disk.
    pub fn resolved_token(&self) -> anyhow::Result<Option<String>> {
        let raw = self
            .api_token
            .clone()
            .or_else(|| std::env::var("LISTEN_API_TOKEN").ok());
        match raw {
            Some(ref s) if s.starts_with("op://") => Ok(Some(resolve_op_reference(s)?)),
            other => Ok(other),
        }
    }

    pub fn endpoint_or_default(&self) -> &str {
        self.endpoint.as_deref().unwrap_or(Self::DEFAULT_ENDPOINT)
    }
}

fn resolve_op_reference(reference: &str) -> anyhow::Result<String> {
    let output = std::process::Command::new("op")
        .args(["read", "--no-newline", reference])
        .output()
        .map_err(|e| {
            anyhow::anyhow!("failed to invoke `op` (is the 1Password CLI installed?): {e}")
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`op read {reference}` failed: {}", stderr.trim());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let Some(config_dir) = dirs::config_dir() else {
            return Ok(Self::default());
        };
        let path = config_dir.join("radi").join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn output_dir_or_default(&self) -> PathBuf {
        self.output_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> anyhow::Result<Config> {
        let config: Config = toml::from_str(s)?;
        Ok(config)
    }

    #[test]
    fn test_default_config_has_no_output_dir() {
        let config = Config::default();
        assert_eq!(config.output_dir, None);
    }

    #[test]
    fn test_parse_valid_toml() {
        let config = parse("output_dir = \"/tmp/podcasts\"").unwrap();
        assert_eq!(config.output_dir, Some(PathBuf::from("/tmp/podcasts")));
    }

    #[test]
    fn test_parse_empty_toml() {
        let config = parse("").unwrap();
        assert_eq!(config.output_dir, None);
    }

    #[test]
    fn test_parse_malformed_toml_is_err() {
        let result = parse("output_dir = [broken");
        assert!(result.is_err());
    }

    #[test]
    fn test_output_dir_or_default_with_none() {
        let config = Config::default();
        assert_eq!(config.output_dir_or_default(), PathBuf::from("."));
    }

    #[test]
    fn test_output_dir_or_default_with_some() {
        let config = Config {
            output_dir: Some(PathBuf::from("/tmp/out")),
            listen: None,
        };
        assert_eq!(config.output_dir_or_default(), PathBuf::from("/tmp/out"));
    }

    #[test]
    fn test_parse_listen_section() {
        let config = parse(
            r#"
            [listen]
            podcast_id = "pod123"
            api_token = "tok"
            endpoint = "https://example.test/graphql"
            "#,
        )
        .unwrap();
        let listen = config.listen.expect("listen section");
        assert_eq!(listen.podcast_id, "pod123");
        assert_eq!(listen.api_token.as_deref(), Some("tok"));
        assert_eq!(
            listen.endpoint.as_deref(),
            Some("https://example.test/graphql")
        );
    }

    #[test]
    fn test_listen_endpoint_or_default() {
        let listen = ListenConfig {
            podcast_id: "p".into(),
            api_token: None,
            endpoint: None,
        };
        assert_eq!(listen.endpoint_or_default(), ListenConfig::DEFAULT_ENDPOINT);
    }

    #[test]
    fn test_listen_resolved_token_prefers_config() {
        let listen = ListenConfig {
            podcast_id: "p".into(),
            api_token: Some("from_config".into()),
            endpoint: None,
        };
        assert_eq!(
            listen.resolved_token().unwrap().as_deref(),
            Some("from_config")
        );
    }
}
