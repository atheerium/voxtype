use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub groq_api_key: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
    /// Force backend: "auto" (detect), "x11", or "wayland"
    pub backend: Option<String>,
    /// PulseAudio source name or device (e.g. "default", "alsa_input.usb-...")
    pub audio_source: Option<String>,
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

        Ok(Config { groq_api_key: None, model: None, language: None, backend: None, audio_source: None })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_assignment() {
        assert_eq!(
            parse_env_assignment("GROQ_API_KEY=gsk_abc", "GROQ_API_KEY"),
            Some("gsk_abc".to_string())
        );
        assert_eq!(
            parse_env_assignment("export GROQ_API_KEY=\"gsk_abc\"", "GROQ_API_KEY"),
            Some("gsk_abc".to_string())
        );
        assert_eq!(
            parse_env_assignment("export GROQ_API_KEY='gsk_abc'", "GROQ_API_KEY"),
            Some("gsk_abc".to_string())
        );
        assert_eq!(
            parse_env_assignment("export GROQ_API_KEY=gsk_abc # comment", "GROQ_API_KEY"),
            Some("gsk_abc".to_string())
        );
        assert_eq!(
            parse_env_assignment("export OTHER=1 GROQ_API_KEY=gsk_abc", "GROQ_API_KEY"),
            Some("gsk_abc".to_string())
        );
        assert_eq!(parse_env_assignment("export PATH=/usr/bin", "GROQ_API_KEY"), None);
        assert_eq!(parse_env_assignment("GROQ_API_KEY=", "GROQ_API_KEY"), None);
        assert_eq!(parse_env_assignment("", "GROQ_API_KEY"), None);
    }

    #[test]
    fn parses_toml_config() {
        let cfg: Config = toml::from_str(
            r#"
            groq_api_key = "gsk_abc"
            backend = "wayland"
            language = "en"
            model = "whisper-tiny"
            audio_source = "alsa_input.usb-mic"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.groq_api_key.as_deref(), Some("gsk_abc"));
        assert_eq!(cfg.model(), "whisper-tiny");
        assert_eq!(cfg.language(), Some("en"));
        assert_eq!(cfg.backend.as_deref(), Some("wayland"));
        assert_eq!(cfg.audio_source.as_deref(), Some("alsa_input.usb-mic"));
    }

    #[test]
    fn defaults_when_absent() {
        let cfg = Config {
            groq_api_key: None,
            model: None,
            language: None,
            backend: None,
            audio_source: None,
        };
        assert_eq!(cfg.model(), "whisper-large-v3-turbo");
        assert_eq!(cfg.language(), None);
    }

    #[test]
    fn api_key_resolution_order() {
        // Config file value wins over the environment variable.
        let cfg = Config {
            groq_api_key: Some("gsk_from_file".to_string()),
            model: None,
            language: None,
            backend: None,
            audio_source: None,
        };
        std::env::set_var("GROQ_API_KEY", "gsk_from_env");
        assert_eq!(cfg.groq_api_key().unwrap(), "gsk_from_file");

        // Environment is the fallback when the config omits the key.
        let cfg = Config {
            groq_api_key: None,
            model: None,
            language: None,
            backend: None,
            audio_source: None,
        };
        assert_eq!(cfg.groq_api_key().unwrap(), "gsk_from_env");

        // With no config key and no env var, resolution fails unless the
        // user's shell rc files leak a real key in. Point HOME at an empty
        // dir so this test is deterministic on any machine.
        let empty_home = std::env::temp_dir().join(format!(
            "voxtype-test-home-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&empty_home);
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &empty_home);
        std::env::remove_var("GROQ_API_KEY");
        assert!(cfg.groq_api_key().is_err());
        match original_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&empty_home);
    }
}
