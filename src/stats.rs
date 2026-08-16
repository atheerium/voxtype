use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Per-provider usage statistics, persisted to disk as JSON.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    pub providers: BTreeMap<String, ProviderStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderStats {
    /// Total API calls attempted (not just fallbacks — every call)
    pub calls: u64,
    /// Calls that returned valid text
    pub successes: u64,
    /// Calls that errored
    pub failures: u64,
    /// Total latency in milliseconds across all calls
    pub total_latency_ms: u64,
    /// Total characters of text returned across all successful calls
    pub total_chars: u64,
}

impl Stats {
    pub fn load() -> Self {
        match stats_path() {
            Ok(path) if path.exists() => {
                let content = fs::read_to_string(&path).unwrap_or_default();
                serde_json::from_str(&content).unwrap_or_default()
            }
            _ => Stats::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = stats_path()?;
        let dir = path.parent().unwrap();
        fs::create_dir_all(dir)?;
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    /// Record a successful transcription for a provider.
    /// `latency_ms` is how long the API call took.
    /// `char_count` is the number of characters returned.
    pub fn record_success(&mut self, provider: &str, latency_ms: u64, char_count: usize) {
        let stats = self.providers.entry(provider.to_string()).or_default();
        stats.calls += 1;
        stats.successes += 1;
        stats.total_latency_ms += latency_ms;
        stats.total_chars += char_count as u64;
    }

    /// Record a failed attempt for a provider.
    pub fn record_failure(&mut self, provider: &str, latency_ms: u64) {
        let stats = self.providers.entry(provider.to_string()).or_default();
        stats.calls += 1;
        stats.failures += 1;
        stats.total_latency_ms += latency_ms;
    }

    /// Print a summary table of all providers.
    pub fn print_table(&self) {
        println!("voxtype provider statistics\n");

        let mut rows: Vec<(&String, &ProviderStats)> = self.providers.iter().collect();
        rows.sort_by(|a, b| b.1.calls.cmp(&a.1.calls));

        println!(
            "{:<12} {:>6} {:>6} {:>6} {:>10} {:>10} {:>12} {:>12}",
            "Provider", "Calls", "OK", "Fail", "Avg(ms)", "Avg(chars)", "Success%", "Total(ms)"
        );
        println!("{}", "-".repeat(82));

        for (name, s) in &rows {
            let avg_ms = if s.calls > 0 {
                s.total_latency_ms / s.calls
            } else {
                0
            };
            let avg_chars = if s.successes > 0 {
                s.total_chars / s.successes
            } else {
                0
            };
            let success_pct = if s.calls > 0 {
                (s.successes * 100) / s.calls
            } else {
                0
            };
            println!(
                "{:<12} {:>6} {:>6} {:>6} {:>10} {:>10} {:>11}% {:>12}",
                name, s.calls, s.successes, s.failures, avg_ms, avg_chars, success_pct,
                s.total_latency_ms
            );
        }

        if rows.is_empty() {
            println!("No statistics recorded yet. Use voxtype to start transcribing.");
        }
    }
}

fn stats_path() -> Result<PathBuf> {
    let data_dir = dirs::data_dir().context("Cannot determine data directory")?;
    Ok(data_dir.join("voxtype").join("stats.json"))
}
