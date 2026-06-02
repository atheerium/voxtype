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
        if let Some(key) = read_key_from_shell_rc() {
            return Ok(key);
        }
        anyhow::bail!(
            "No Groq API key found. Set GROQ_API_KEY in your shell, or add groq_api_key to {}",
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

fn read_key_from_shell_rc() -> Option<String> {
    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".bashrc"),
        home.join(".zshrc"),
        home.join(".bash_profile"),
        home.join(".profile"),
        home.join(".zprofile"),
    ];
    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("export") {
                    if let Some(rest) = trimmed.strip_prefix("export") {
                        let rest = rest.trim();
                        if let Some(val) = parse_env_assignment(rest, "GROQ_API_KEY") {
                            return Some(val);
                        }
                    }
                } else if let Some(val) = parse_env_assignment(trimmed, "GROQ_API_KEY") {
                    return Some(val);
                }
            }
        }
    }
    None
}

fn parse_env_assignment(line: &str, var: &str) -> Option<String> {
    let prefix = format!("{}=", var);
    if let Some(idx) = line.find(&prefix) {
        let after = &line[idx + prefix.len()..];
        let val = after.split_whitespace().next()?;
        let val = val.trim_matches('"').trim_matches('\'');
        if !val.is_empty() {
            return Some(val.to_string());
        }
    }
    None
}
