# Building voxtype: A Free, Open-Source Voice Dictation Tool for Linux

*August 25, 2026 · Atheerium*

---

## TL;DR

I built **voxtype** — a free, open-source voice-to-text dictation tool for Linux that types your spoken words into any application. Press `Ctrl+Space`, speak, press `Ctrl+Space` again, and your words appear. It's MIT-licensed, the whole binary is ~2.4 MB, and there are zero accounts, subscriptions, or vendor clouds involved.

[voxtype on GitHub](https://github.com/atheerium/voxtype) | [Install in one command](https://github.com/atheerium/voxtype#install-in-one-command)

---

## Why I built voxtype

Voice dictation on Linux has a problem: the best tool, **Wispr Flow**, is macOS/Windows-only and subscription-based. On Linux, you're stuck with either expensive cloud services, complicated local setups, or nothing at all.

I wanted something that:
- Works on both X11 and Wayland
- Is 100% free with no subscription
- Is fully open-source (MIT license)
- Sends audio only to the provider you choose
- Is a tiny native binary, not an Electron app

So I built voxtype. Here's what it looks like:

```
Ctrl+Space → voxtype CLI → SIGUSR1 → voxtype daemon (background)
                                      ├── ffmpeg records mic → /tmp/voxtype.mp3
                                      └── Ctrl+Space again → Groq API → clipboard → auto-paste
```

## Architecture

voxtype is built around a background daemon pattern:

1. **Hotkey listener (CLI):** A lightweight binary that, when triggered, signals the daemon via `SIGUSR1`.
2. **Audio recording:** The daemon uses `ffmpeg` to record your microphone into a temp file at 16kHz mono.
3. **Transcription:** Audio is sent to Groq's Whisper API (configurable — any OpenAI-compatible endpoint works).
4. **Text output:** The transcribed text is copied to the clipboard and pasted into the focused application.

The daemon pattern means voxtype uses near-zero resources when idle — no always-listening process, no background CPU usage.

### X11 vs Wayland: the paste problem

The hardest part wasn't the transcription — it was getting text into the focused application reliably.

On **X11**, you can use `xdotool` to simulate keyboard input or paste from clipboard. But you need to detect whether the focused window is a terminal (which needs `Ctrl+Shift+V`) or a GUI app (which needs `Ctrl+V`).

On **Wayland**, clipboard access is mediated by the compositor. You need `wl-clipboard` for clipboard operations and `wtype` for keyboard simulation. Each compositor (Sway, Hyprland, KDE, GNOME) has different paste shortcuts.

voxtype auto-detects your environment and picks the right paste strategy. Here's a simplified view of the paste-key selection logic:

```rust
pub fn paste_keys_for_known_targets(target: &WmClass) -> Vec<KeyCombo> {
    match target {
        WmClass::Terminal => vec![KeyCombo::CtrlShiftV],
        WmClass::Browser => vec![KeyCombo::CtrlV],
        WmClass::Editor => vec![KeyCombo::CtrlShiftV, KeyCombo::CtrlV],
        _ => vec![KeyCombo::CtrlV],
    }
}
```

### The provider fallback system

One frustration with voice dictation tools is rate limits. If your primary transcription provider throttles you, dictation just... stops.

voxtype implements a **provider fallback chain**: Deepgram → Mistral → Groq. Each provider is tried in order, and the results are cached so the next transcription uses the fastest provider first.

The stats module tracks per-provider latency and reliability, and promotes winners:

```rust
// Providers are ranked by reliability (success rate) then latency
// A proven provider beats an unknown one even if slower
pub fn rank_providers(stats: &ProviderStats) -> Vec<String> {
    stats.providers.iter()
        .sorted_by(|a, b| b.reliability.cmp(&a.reliability)
            .then(a.avg_latency.cmp(&b.avg_latency)))
        .map(|p| p.name.clone())
        .collect()
}
```

## Technical decisions

### Why Rust?

- **Safety:** No segfaults in a daemon that runs in the background
- **Size:** With `panic=abort`, `lto=true`, `codegen-units=1`, and `strip=true`, the release binary is ~2.4 MB
- **Zero runtime:** No Node.js, no Python interpreter, no Electron overhead
- **Cargo:** Publishing to crates.io took 5 minutes and gave me immediate download metrics

### Why Whisper via API instead of local models?

Whisper models are large (700 MB for large-v3) and require a GPU or significant CPU time to transcribe. By using the API (Groq's Whisper endpoint), voxtype:
- Has a 2.4 MB binary (no model weights bundled)
- Transcribes in under 2 seconds
- Costs pennies per use (Groq's free tier is 1,000+ hours/month)

The roadmap includes offline transcription via `whisper.cpp` for users who want local-only processing.

### The config resolution chain

voxtype resolves settings in a specific order:

1. `config.toml` file (`~/.config/voxtype/config.toml`)
2. `GROQ_API_KEY` environment variable
3. Shell rc files (`.bashrc`, `.zshrc`, etc.)

This follows the principle of least surprise — you set your API key once during install and never need to think about it again.

## Lessons learned

1. **Wayland is still fragmented.** Each compositor has its own quirks. Auto-detection works ~80% of the time. The other 20% hits the troubleshooting guide.

2. **Process hygiene matters for daemons.** voxtype stores its PID in `/tmp/voxtype.pid`, verifies PIDs against `/proc` before killing orphans, and cleans up its own lock/audio/temp files on exit. This prevented several "daemon not responding" bugs during development.

3. **The `exclude` list in Cargo.toml matters for crates.io.** My first publish attempt included 50 MB of `.cargo-cache` and `.github` workflows. Fixed by adding them to the exclude list:

   ```toml
   exclude = [".github", "docs", ".cargo-cache"]
   ```

4. **Clipboard-first design prevents most paste failures.** Even if the automatic paste misses (wrong shortcut, focused app doesn't accept it), the text is always on the clipboard. Users can `Ctrl+V` manually as a fallback.

## Roadmap

- Voice editing commands (select, delete, replace) like Wispr Flow
- Offline transcription via `whisper.cpp`
- Packaged releases (deb, rpm, AUR, Homebrew)
- System tray indicator
- Multiple backend providers (self-hosted Whisper, AssemblyAI, etc.)

## Try it

```bash
curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash
# Set your Groq API key (free at console.groq.com)
# Press Ctrl+Space, speak, press Ctrl+Space again. Done.
```

Contributions welcome — the project is [MIT-licensed](https://github.com/atheerium/voxtype/blob/main/LICENSE) and I'm actively looking for help with Wayland integration testing, packaging, and provider abstraction. See [CONTRIBUTING.md](https://github.com/atheerium/voxtype/blob/main/CONTRIBUTING.md) for the workflow.

---

*[Atheerium](https://atheerium.com) builds open-source tools for Linux. If this saved you time, [buy me a coffee](https://ko-fi.com/atheerium) or star the repo.*