use crate::config::Config;
use anyhow::{Context, Result};
use std::io::Write;

/// Lightweight interactive configuration for the default speech provider.
/// Simple prompt-based selector — no TUI libraries needed.
pub fn run_configure() -> Result<()> {
    let config = Config::load()?;

    let providers = ["auto", "deepgram", "mistral", "groq"];
    let current = config.default_provider();

    println!("libretype — configure default speech provider\\n");

    println!("Current default: {}", current);
    println!();

    // Show which providers have keys configured
    for (i, p) in providers.iter().enumerate() {
        let has_key = match *p {
            "auto" => true,
            "groq" => {
                config
                    .groq_api_key
                    .as_ref()
                    .map(|k| !k.is_empty())
                    .unwrap_or(false)
                    || std::env::var("GROQ_API_KEY")
                        .map(|k| !k.is_empty())
                        .unwrap_or(false)
            }
            "deepgram" => {
                config
                    .deepgram_api_key
                    .as_ref()
                    .map(|k| !k.is_empty())
                    .unwrap_or(false)
                    || std::env::var("DEEPGRAM_API_KEY")
                        .map(|k| !k.is_empty())
                        .unwrap_or(false)
            }
            "mistral" => {
                config
                    .mistral_api_key
                    .as_ref()
                    .map(|k| !k.is_empty())
                    .unwrap_or(false)
                    || std::env::var("MISTRAL_API_KEY")
                        .map(|k| !k.is_empty())
                        .unwrap_or(false)
            }
            _ => false,
        };
        let key_status = if has_key { "✓" } else { "✗" };
        if *p == "auto" {
            println!("  [{}] {} — fallback chain (key: {})", i + 1, p, key_status);
        } else {
            println!("  [{}] {} (key: {})", i + 1, p, key_status);
        }
    }
    println!();

    // Prompt for input
    print!("Select provider [1-4] (default: {}): ", current);
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim();

    if input.is_empty() {
        println!(
            "No changes made (press Enter to keep current: {}).",
            current
        );
        return Ok(());
    }

    let idx: usize = input.parse().context("Please enter a number 1-4")?;
    if idx < 1 || idx > providers.len() {
        anyhow::bail!("Please select a number between 1 and {}", providers.len());
    }

    let selected = providers[idx - 1];

    // Check if the selected provider has a key (auto always works)
    if selected != "auto" {
        let has_key = match selected {
            "groq" => {
                config
                    .groq_api_key
                    .as_ref()
                    .map(|k| !k.is_empty())
                    .unwrap_or(false)
                    || std::env::var("GROQ_API_KEY")
                        .map(|k| !k.is_empty())
                        .unwrap_or(false)
            }
            "deepgram" => {
                config
                    .deepgram_api_key
                    .as_ref()
                    .map(|k| !k.is_empty())
                    .unwrap_or(false)
                    || std::env::var("DEEPGRAM_API_KEY")
                        .map(|k| !k.is_empty())
                        .unwrap_or(false)
            }
            "mistral" => {
                config
                    .mistral_api_key
                    .as_ref()
                    .map(|k| !k.is_empty())
                    .unwrap_or(false)
                    || std::env::var("MISTRAL_API_KEY")
                        .map(|k| !k.is_empty())
                        .unwrap_or(false)
            }
            _ => true,
        };
        if !has_key {
            let env_var = match selected {
                "groq" => "GROQ_API_KEY",
                "deepgram" => "DEEPGRAM_API_KEY",
                "mistral" => "MISTRAL_API_KEY",
                _ => "",
            };
            anyhow::bail!(
                "You selected '{}' but no API key is configured.\n\
                 Add it to ~/.config/voxtype/config.toml or export {} in your shell.",
                selected,
                env_var
            );
        }
    }

    // Save the config
    let mut cfg = config.clone();
    cfg.default_provider = if selected == "auto" {
        None
    } else {
        Some(selected.to_string())
    };
    cfg.save()?;

    println!("\nDefault provider set to: {}", selected);
    println!("Config saved to ~/.config/libretype/config.toml");
    Ok(())
}
