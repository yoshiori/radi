use std::path::PathBuf;

#[derive(Debug, Default, serde::Deserialize, PartialEq)]
pub struct Config {
    pub output_dir: Option<PathBuf>,
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
        };
        assert_eq!(config.output_dir_or_default(), PathBuf::from("/tmp/out"));
    }
}
