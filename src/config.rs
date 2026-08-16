use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub groq_api_key: Option<String>,
    pub deepgram_api_key: Option<String>,
    pub mistral_api_key: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
    /// Force backend: "auto" (detect), "x11", or "wayland"
    pub backend: Option<String>,
    /// PulseAudio source name or device (e.g. "default", "alsa_input.usb-...")
    pub audio_source: Option<String>,
    /// Default speech provider: "deepgram", "mistral", "groq", or "auto" (fallback chain)
    pub default_provider: Option<String>,
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

        Ok(Config {
            groq_api_key: None,
            deepgram_api_key: None,
            mistral_api_key: None,
            model: None,
            language: None,
            backend: None,
            audio_source: None,
            default_provider: None,
        })
    }

    pub fn groq_api_key(&self) -> Result<String> {
        resolve_key(&self.groq_api_key, "GROQ_API_KEY", "Groq")
    }

    pub fn deepgram_api_key(&self) -> Result<String> {
        resolve_key(&self.deepgram_api_key, "DEEPGRAM_API_KEY", "Deepgram")
    }

    pub fn mistral_api_key(&self) -> Result<String> {
        resolve_key(&self.mistral_api_key, "MISTRAL_API_KEY", "Mistral")
    }

    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or("whisper-large-v3-turbo")
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    pub fn default_provider(&self) -> &str {
        self.default_provider
            .as_deref()
            .unwrap_or("auto")
    }

    /// Write the current config back to the config file as TOML.
    pub fn save(&self) -> Result<()> {
        let config_path = config_path()?;
        let parent = config_path
            .parent()
            .context("Config path has no parent directory")?;
        fs::create_dir_all(parent)?;

        // Serialize to TOML, preserving field order as written above.
        let toml = config_toml(self);
        fs::write(&config_path, toml)?;
        Ok(())
    }
}

fn config_toml(cfg: &Config) -> String {
    let mut out = String::new();
    macro_rules! opt {
        ($name:expr, $val:expr) => {
            if let Some(v) = $val {
                out.push_str(&format!("{} = {:?}\n", $name, v));
            }
        };
    }
    opt!("groq_api_key", &cfg.groq_api_key);
    opt!("deepgram_api_key", &cfg.deepgram_api_key);
    opt!("mistral_api_key", &cfg.mistral_api_key);
    opt!("model", &cfg.model);
    opt!("language", &cfg.language);
    opt!("backend", &cfg.backend);
    opt!("audio_source", &cfg.audio_source);
    opt!("default_provider", &cfg.default_provider);
    out
}

/// Generic key resolver: config file value wins, then env var, then shell rc.
/// `provider` is used in error messages so each provider has a clear hint.
fn resolve_key(file_value: &Option<String>, env_var: &str, provider: &str) -> Result<String> {
    if let Some(ref key) = file_value {
        if !key.is_empty() {
            return Ok(key.clone());
        }
    }
    if let Ok(key) = std::env::var(env_var) {
        if !key.is_empty() {
            return Ok(key);
        }
    }
    anyhow::bail!(
        "No {} API key found. Set {} in your shell, or add {}_api_key to {}",
        provider,
        env_var,
        provider.to_lowercase(),
        config_path()?.display()
    )
}

fn config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Cannot determine config directory")?;
    Ok(config_dir.join("voxtype").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            parse_env_assignment("export DEEPGRAM_API_KEY='dg_abc'", "DEEPGRAM_API_KEY"),
            Some("dg_abc".to_string())
        );
        assert_eq!(
            parse_env_assignment("export MISTRAL_API_KEY=abc123 # comment", "MISTRAL_API_KEY"),
            Some("abc123".to_string())
        );
        assert_eq!(
            parse_env_assignment("export OTHER=1 GROQ_API_KEY=gsk_abc", "GROQ_API_KEY"),
            Some("gsk_abc".to_string())
        );
        assert_eq!(
            parse_env_assignment("export PATH=/usr/bin", "GROQ_API_KEY"),
            None
        );
        assert_eq!(parse_env_assignment("GROQ_API_KEY=", "GROQ_API_KEY"), None);
        assert_eq!(parse_env_assignment("", "GROQ_API_KEY"), None);
    }

    #[test]
    fn parses_toml_config() {
        let cfg: Config = toml::from_str(
            r#"
            groq_api_key = "gsk_abc"
            deepgram_api_key = "dg_abc"
            mistral_api_key = "m_abc"
            backend = "wayland"
            language = "en"
            model = "whisper-tiny"
            audio_source = "alsa_input.usb-mic"
            default_provider = "deepgram"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.groq_api_key.as_deref(), Some("gsk_abc"));
        assert_eq!(cfg.deepgram_api_key.as_deref(), Some("dg_abc"));
        assert_eq!(cfg.mistral_api_key.as_deref(), Some("m_abc"));
        assert_eq!(cfg.model(), "whisper-tiny");
        assert_eq!(cfg.language(), Some("en"));
        assert_eq!(cfg.backend.as_deref(), Some("wayland"));
        assert_eq!(cfg.audio_source.as_deref(), Some("alsa_input.usb-mic"));
        assert_eq!(cfg.default_provider(), "deepgram");
    }

    #[test]
    fn defaults_when_absent() {
        let cfg = Config {
            groq_api_key: None,
            deepgram_api_key: None,
            mistral_api_key: None,
            model: None,
            language: None,
            backend: None,
            audio_source: None,
            default_provider: None,
        };
        assert_eq!(cfg.model(), "whisper-large-v3-turbo");
        assert_eq!(cfg.language(), None);
        assert_eq!(cfg.default_provider(), "auto");
    }

    #[test]
    fn api_key_resolution_order() {
        // Config file value wins over the environment variable.
        let cfg = Config {
            groq_api_key: Some("gsk_from_file".to_string()),
            deepgram_api_key: None,
            mistral_api_key: None,
            model: None,
            language: None,
            backend: None,
            audio_source: None,
            default_provider: None,
        };
        std::env::set_var("GROQ_API_KEY", "gsk_from_env");
        assert_eq!(cfg.groq_api_key().unwrap(), "gsk_from_file");

        // Environment is the fallback when the config omits the key.
        let cfg = Config {
            groq_api_key: None,
            deepgram_api_key: None,
            mistral_api_key: None,
            model: None,
            language: None,
            backend: None,
            audio_source: None,
            default_provider: None,
        };
        assert_eq!(cfg.groq_api_key().unwrap(), "gsk_from_env");

        // With no config key and no env var, resolution fails unless the
        // user's shell rc files leak a real key in. Point HOME at an empty
        // dir so this test is deterministic on any machine.
        let empty_home =
            std::env::temp_dir().join(format!("voxtype-test-home-{}", std::process::id()));
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
