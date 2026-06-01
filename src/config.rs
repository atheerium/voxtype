use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub groq_api_key: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = config_path()?;

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read config at {}", config_path.display()))?;
            let config: Config =
                toml::from_str(&content).with_context(|| "Failed to parse config TOML")?;
            return Ok(config);
        }

        Ok(Config { groq_api_key: None, model: None, language: None })
    }

    pub fn groq_api_key(&self) -> Result<String> {
        if let Some(ref key) = self.groq_api_key {
            if !key.is_empty() {
                return Ok(key.clone());
            }
        }
        if let Ok(key) = std::env::var("GROQ_API_KEY") {
            if !key.is_empty() {
                return Ok(key);
            }
        }
        anyhow::bail!(
            "No Groq API key found. Set GROQ_API_KEY env var or add groq_api_key to {}",
            config_path()?.display()
        )
    }

    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or("whisper-large-v3-turbo")
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }
}

fn config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Cannot determine config directory")?;
    Ok(config_dir.join("voxtype").join("config.toml"))
}
