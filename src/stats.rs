use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
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

    /// Rank available providers by historical reliability (primary) and
    /// average latency (secondary). Reliability uses Laplace (add-one)
    /// smoothing so that providers with few calls are not over-ranked:
    /// `(successes + 1) / (calls + 2)`. A provider with zero recorded
    /// calls gets a default rate of 0.5 (and a pessimistic latency), so
    /// proven providers always sort ahead of unknowns.
    ///
    /// `available` is the list of providers that have a configured API key,
    /// in the user's preferred default order. That order is preserved as the
    /// final tiebreaker so that with no data the original chain is used.
    pub fn ranked_providers(&self, available: &[String]) -> Vec<String> {
        let mut scored: Vec<(String, f64, u64, usize)> = available
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (rate, avg_latency) = self
                    .providers
                    .get(p)
                    .filter(|s| s.calls > 0)
                    .map(|s| {
                        let rate = (s.successes as f64 + 1.0) / (s.calls as f64 + 2.0);
                        let avg = s.total_latency_ms / s.calls;
                        (rate, avg)
                    })
                    .unwrap_or((0.5, 100_000)); // default: no data → penalized
                (p.clone(), rate, avg_latency, i)
            })
            .collect();

        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal) // reliability desc
                .then_with(|| a.2.cmp(&b.2)) // latency asc
                .then_with(|| a.3.cmp(&b.3)) // original order stable
        });

        scored.into_iter().map(|(p, _, _, _)| p).collect()
    }

    /// Print a summary table of all providers.
    pub fn print_table(&self) {
        println!("voxtype provider statistics\n");

        let mut rows: Vec<(&String, &ProviderStats)> = self.providers.iter().collect();
        rows.sort_by_key(|b| std::cmp::Reverse(b.1.calls));

        println!(
            "{:<12} {:>6} {:>6} {:>6} {:>10} {:>10} {:>12} {:>12}",
            "Provider", "Calls", "OK", "Fail", "Avg(ms)", "Avg(chars)", "Success%", "Total(ms)"
        );
        println!("{}", "-".repeat(82));

        for (name, s) in &rows {
            let avg_ms = s.total_latency_ms.checked_div(s.calls).unwrap_or(0);
            let avg_chars = s.total_chars.checked_div(s.successes).unwrap_or(0);
            let success_pct = (s.successes * 100).checked_div(s.calls).unwrap_or(0);
            println!(
                "{:<12} {:>6} {:>6} {:>6} {:>10} {:>10} {:>11}% {:>12}",
                name,
                s.calls,
                s.successes,
                s.failures,
                avg_ms,
                avg_chars,
                success_pct,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stats() -> Stats {
        Stats {
            providers: BTreeMap::new(),
        }
    }

    fn avail(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_data_preserves_default_order() {
        let stats = make_stats();
        let ranked = stats.ranked_providers(&avail(&["deepgram", "mistral", "groq"]));
        assert_eq!(ranked, vec!["deepgram", "mistral", "groq"]);
    }

    #[test]
    fn reliability_ranks_first_even_if_slower() {
        // deepgram: 9 success, 1 failure → rate = (9+1)/(10+2) = 0.833
        // groq:     4 success, 1 failure → rate = (4+1)/(5+2)  = 0.714
        // Even though groq is faster (20ms vs 100ms), deepgram's higher
        // reliability must rank it first.
        let mut stats = make_stats();
        for _ in 0..9 {
            stats.record_success("deepgram", 100, 10);
        }
        stats.record_failure("deepgram", 100);

        for _ in 0..4 {
            stats.record_success("groq", 20, 10);
        }
        stats.record_failure("groq", 20);

        let ranked = stats.ranked_providers(&avail(&["groq", "deepgram"]));
        assert_eq!(ranked, vec!["deepgram", "groq"]);
    }

    #[test]
    fn latency_breaks_ties_when_reliability_equal() {
        // Both deepgram and groq: 5/5 success → rate = (5+1)/(5+2) = 6/7 ≈ 0.857
        let mut stats = make_stats();
        for _ in 0..5 {
            stats.record_success("deepgram", 100, 10);
            stats.record_success("groq", 30, 10);
        }

        let ranked = stats.ranked_providers(&avail(&["deepgram", "groq"]));
        // Equal reliability → faster (groq) ranks first
        assert_eq!(ranked, vec!["groq", "deepgram"]);
    }

    #[test]
    fn proven_provider_beats_unknown_even_if_slower() {
        let mut stats = make_stats();
        // mistral: 1/1 success, 500ms latency
        stats.record_success("mistral", 500, 10);

        let ranked = stats.ranked_providers(&avail(&["groq", "mistral"]));
        // mistral (rate = 2/3 ≈ 0.667) > groq (rate = 0.5)
        assert_eq!(ranked[0], "mistral");
        assert_eq!(ranked[1], "groq");
    }

    #[test]
    fn single_failure_drops_provider_below_known_good() {
        let mut stats = make_stats();
        // deepgram: 1/1 → rate 2/3 ≈ 0.667
        stats.record_success("deepgram", 50, 10);
        // mistral: 0/1 → rate 1/3 ≈ 0.33
        stats.record_failure("mistral", 50);

        let ranked = stats.ranked_providers(&avail(&["mistral", "deepgram"]));
        assert_eq!(ranked, vec!["deepgram", "mistral"]);
    }

    #[test]
    fn empty_available_returns_empty() {
        let stats = make_stats();
        assert!(stats.ranked_providers(&[]).is_empty());
    }

    #[test]
    fn large_sample_overrides_small_sample() {
        // groq: 1/1 success, very fast
        let mut stats = make_stats();
        stats.record_success("groq", 20, 10);

        // deepgram: 50/51 success (rate = 51/53 ≈ 0.962), slower
        for _ in 0..50 {
            stats.record_success("deepgram", 200, 10);
        }
        stats.record_failure("deepgram", 200);

        let ranked = stats.ranked_providers(&avail(&["groq", "deepgram"]));
        // deepgram (0.962) >> groq (0.667)
        assert_eq!(ranked, vec!["deepgram", "groq"]);
    }
}
