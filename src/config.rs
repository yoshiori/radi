use std::path::PathBuf;

#[derive(Debug, Default, serde::Deserialize, PartialEq)]
pub struct Config {
    pub output_dir: Option<PathBuf>,
    pub listen: Option<ListenConfig>,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq)]
pub struct ListenConfig {
    pub podcast_id: String,
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    /// 1Password account (sign-in address, email, or ID) passed to
    /// `op read --account`. Needed when the CLI is signed in to several
    /// accounts, because `op` otherwise searches only the default one.
    #[serde(default)]
    pub op_account: Option<String>,
}

impl ListenConfig {
    pub const DEFAULT_ENDPOINT: &'static str = "https://listen.style/graphql";

    /// Token from config, falling back to the LISTEN_API_TOKEN env var.
    ///
    /// Values prefixed with `op://` are resolved via the 1Password CLI
    /// (`op read <ref>`), so config.toml can reference a vault item without
    /// storing the secret on disk. Errors out when neither source supplies
    /// a value; the message tells the user exactly where to put one.
    pub fn required_token(&self) -> anyhow::Result<String> {
        let raw = self
            .api_token
            .clone()
            .or_else(|| std::env::var("LISTEN_API_TOKEN").ok())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "LISTEN API token not configured (set [listen].api_token in config.toml or LISTEN_API_TOKEN env var)"
                )
            })?;
        if raw.starts_with("op://") {
            resolve_op_reference(&raw, self.op_account.as_deref())
        } else {
            Ok(raw)
        }
    }

    pub fn endpoint_or_default(&self) -> &str {
        self.endpoint.as_deref().unwrap_or(Self::DEFAULT_ENDPOINT)
    }
}

fn resolve_op_reference(reference: &str, account: Option<&str>) -> anyhow::Result<String> {
    let output = std::process::Command::new("op")
        .args(op_read_args(reference, account))
        .output()
        .map_err(|e| {
            anyhow::anyhow!("failed to invoke `op` (is the 1Password CLI installed?): {e}")
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "`op read {reference}` failed: {} ({})",
            stderr.trim(),
            op_failure_hint(account)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn op_read_args<'a>(reference: &'a str, account: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec!["read", "--no-newline"];
    if let Some(account) = account {
        args.extend(["--account", account]);
    }
    args.push(reference);
    args
}

fn op_failure_hint(account: Option<&str>) -> String {
    match account {
        Some(account) => format!(
            "check that the item exists in 1Password account `{account}` and that `op account list` shows it"
        ),
        None => "if `op account list` shows several accounts, set [listen].op_account in config.toml to the one holding the item".to_string(),
    }
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
            op_account: None,
        };
        assert_eq!(listen.endpoint_or_default(), ListenConfig::DEFAULT_ENDPOINT);
    }

    #[test]
    fn test_listen_required_token_prefers_config() {
        let listen = ListenConfig {
            podcast_id: "p".into(),
            api_token: Some("from_config".into()),
            endpoint: None,
            op_account: None,
        };
        assert_eq!(listen.required_token().unwrap(), "from_config");
    }

    #[test]
    fn test_parse_listen_op_account() {
        let config = parse(
            r#"
            [listen]
            podcast_id = "pod123"
            api_token = "op://Private/item/credential"
            op_account = "my.1password.com"
            "#,
        )
        .unwrap();
        let listen = config.listen.expect("listen section");
        assert_eq!(listen.op_account.as_deref(), Some("my.1password.com"));
    }

    #[test]
    fn test_parse_listen_op_account_defaults_to_none() {
        let config = parse(
            r#"
            [listen]
            podcast_id = "pod123"
            "#,
        )
        .unwrap();
        assert_eq!(config.listen.expect("listen section").op_account, None);
    }

    #[test]
    fn test_op_read_args_without_account() {
        assert_eq!(
            op_read_args("op://V/i/f", None),
            vec!["read", "--no-newline", "op://V/i/f"]
        );
    }

    #[test]
    fn test_op_read_args_with_account() {
        assert_eq!(
            op_read_args("op://V/i/f", Some("my.1password.com")),
            vec![
                "read",
                "--no-newline",
                "--account",
                "my.1password.com",
                "op://V/i/f"
            ]
        );
    }

    #[test]
    fn test_op_failure_hint_suggests_op_account_when_unset() {
        assert!(op_failure_hint(None).contains("[listen].op_account"));
    }

    #[test]
    fn test_op_failure_hint_names_configured_account() {
        assert!(op_failure_hint(Some("my.1password.com")).contains("my.1password.com"));
    }
}
