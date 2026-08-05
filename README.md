<p align="center">
  <img src="docs/voxtype-banner.svg" width="100%" alt="voxtype — free, open-source voice-to-text dictation for Linux. Press Ctrl+Space, speak, done.">
</p>

# voxtype — Free, Open-Source Voice-to-Text Dictation for Linux

<p align="center">
  The <strong>Wispr Flow alternative</strong> that is free, private, and Linux-first.<br>
  Press <kbd>Ctrl</kbd>+<kbd>Space</kbd>, speak, and your words are typed into any app.
</p>

<p align="center">
  <a href="https://github.com/atheerium/voxtype/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-green.svg" alt="MIT license"></a>
  <a href="https://github.com/atheerium/voxtype/releases"><img src="https://img.shields.io/github/v/release/atheerium/voxtype?sort=semver&label=release" alt="Latest release"></a>
  <a href="https://github.com/atheerium/voxtype/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/atheerium/voxtype/ci.yml?branch=main&label=CI" alt="CI status"></a>
  <img src="https://img.shields.io/badge/size-2.4%20MB-blue.svg" alt="~2.4 MB binary">
  <img src="https://img.shields.io/badge/platform-Linux%20(X11%20%2B%20Wayland)-blueviolet.svg" alt="Linux X11 and Wayland">
  <img src="https://img.shields.io/badge/dictation-Whisper%20via%20Groq-cyan.svg" alt="Whisper via Groq">
  <a href="https://github.com/atheerium/voxtype/stargazers"><img src="https://img.shields.io/github/stars/atheerium/voxtype?style=social" alt="GitHub stars"></a>
  <a href="https://ko-fi.com/atheerium"><img src="https://img.shields.io/badge/Buy%20me%20a%20coffee-%F0%9F%8E%81%20ko--fi.com/atheerium-ff5e5b.svg" alt="Support on Ko-fi"></a>
</p>

---

## 🚀 Install in one command

```bash
curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash
```

That's it. The installer detects your display server and compositor, installs
dependencies, downloads the prebuilt binary (or builds from source), writes
your config, sets up autostart, binds <kbd>Ctrl</kbd>+<kbd>Space</kbd>, and
starts the daemon.

**You only need a Groq API key** (free at [console.groq.com](https://console.groq.com)) —
set it once: `export GROQ_API_KEY="gsk_..."` or pass `--api-key` to the installer.

> Try it without changing anything: add `--dry-run` to preview every step.
> Want the very latest installer from `main`? Use
> `https://raw.githubusercontent.com/atheerium/voxtype/main/install.sh` instead.

---

## What is voxtype?

**voxtype is a free, open-source, privacy-friendly voice-to-text (speech-to-text /
dictation) tool for Linux.** It turns your microphone into a keyboard: you press a
global hotkey, speak naturally, and voxtype transcribes your words and types them
into whatever app has focus — a terminal, editor, browser, or chat window.

It is built around a single idea: **dictation should be one keystroke away, work
everywhere, and cost nothing.** No Electron app, no account, no subscription, no
vendor lock-in. The whole binary is ~2.4 MB and the source is MIT-licensed.

- **Linux-first.** Runs on X11 and Wayland (Sway, Hyprland, KDE Plasma, GNOME, XFCE, i3, and more).
- **Wispr Flow alternative.** Wispr Flow is popular but macOS/Windows-only, closed-source, and subscription-based. voxtype is the free, open-source equivalent Linux users have been missing.
- **Private by default.** Audio goes directly from your machine to the speech provider you choose (Groq's Whisper API by default). Bring your own API key — no voxtype account, no telemetry, no data brokering.
- **Fast.** Record locally with ffmpeg, transcribe with OpenAI-compatible Whisper models, and paste results in seconds.

## Why use voxtype?

| Problem | voxtype's answer |
|---|---|
| Wispr Flow doesn't run on Linux | Built for X11 **and** Wayland, first class |
| Dictation apps cost $15–30/month | **100% free**, MIT licensed, no subscriptions |
| Closed-source apps you can't audit | Entire source is readable and auditable |
| Privacy-opaque clouds | Bring your own API key; no voxtype servers |
| Heavy Electron apps | A single ~2.4 MB native binary |
| Terminal paste is a pain | Auto-detects terminal vs GUI, picks the right shortcut |

## Features

- 🎙️ **One-hotkey dictation** — <kbd>Ctrl</kbd>+<kbd>Space</kbd> to record, <kbd>Ctrl</kbd>+<kbd>Space</kbd> again to paste
- 🖥️ **X11 + Wayland support** — auto-detects compositor (Sway, Hyprland, KDE, GNOME) and chooses correct paste shortcuts
- 📋 **Clipboard-first design** — text is always copied, so even if keyboard paste misses, manual paste works
- 🧠 **Modern Whisper models** — `whisper-large-v3-turbo` by default, configurable (any Groq model)
- 🔑 **Bring your own key** — Groq API key from env var or `config.toml`
- 🪶 **~2.4 MB native binary** — no runtime, no Electron, `panic=abort`, fully stripped
- 🛡️ **Robust daemon** — crash recovery, stale-process cleanup, single-instance guard, graceful degradation when tools are missing
- 🐧 **Distro-agnostic** — installer supports apt, dnf, pacman, zypper, apk, emerge

## How it works

```
Ctrl+Space → voxtype CLI → SIGUSR1 → voxtype daemon (background)
                                      ├── ffmpeg records mic → /tmp/voxtype.mp3
                                      └── Ctrl+Space again → Groq API → clipboard → auto-paste
```

1. You press <kbd>Ctrl</kbd>+<kbd>Space</kbd>. The CLI sends a signal to the background daemon.
2. The daemon records your microphone with **ffmpeg** (16 kHz mono, ~64 kbps) into a temp file.
3. You press <kbd>Ctrl</kbd>+<kbd>Space</kbd> again. The daemon stops recording and uploads the audio to **Groq's Whisper API**.
4. The transcribed text is copied to your clipboard and **pasted into the focused app** automatically.
5. On Wayland, paste uses `wtype` with compositor-aware shortcuts; on X11, `xdotool` detects terminal vs GUI apps.

## voxtype vs Wispr Flow

| | **voxtype** | **Wispr Flow** |
|---|---|---|
| **Price** | Free (MIT license) | Paid subscription |
| **Source** | Open source, auditable | Closed source |
| **Linux support** | ✅ X11 + Wayland | ❌ macOS/Windows only |
| **Install size** | ~2.4 MB binary | Large desktop app |
| **Account / cloud** | None — your API key, your provider | Proprietary cloud service |
| **Privacy model** | You choose the transcription provider | Vendor-controlled |
| **Offline editing commands** | Not yet (roadmap) | Yes |
| **Customization** | Config file, any Whisper model | Limited |

voxtype does **not** yet have Wispr Flow's advanced features — voice editing
commands, custom vocabularies, or offline transcription. It nails the core
loop: *press, speak, done* — everywhere, for free.

## Requirements

- **Linux** with an X11 or Wayland session (works on Sway, Hyprland, KDE, GNOME, XFCE, i3, …)
- **ffmpeg** for audio recording (installed automatically by the installer)
- **A Groq API key** — free tier at [console.groq.com](https://console.groq.com)
- Clipboard tools: X11 needs `xsel`/`xclip` + `xdotool`; Wayland needs `wl-clipboard` + `wtype` (all auto-installed)
- Optional: `notify-send` for desktop notifications, `pactl` for audio detection

## Installation

### Option A: One-command installer (recommended)

```bash
curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash
```

The installer:

1. Detects your display server (X11 / Wayland) and compositor
2. Installs dependencies through your package manager (apt, dnf, pacman, zypper, apk, emerge)
3. Downloads the prebuilt binary from GitHub Releases (falls back to a source build)
4. Writes `~/.config/voxtype/config.toml`
5. Sets up autostart and binds <kbd>Ctrl</kbd>+<kbd>Space</kbd>
6. Starts the daemon

Useful flags:

```bash
# Preview everything without touching your system
curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash -s -- --dry-run

# Provide the API key non-interactively
curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash -s -- --api-key gsk_xxx

# Build from source instead of downloading a binary
curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash -s -- --method source
```

### Option B: Build from source

```bash
git clone https://github.com/atheerium/voxtype.git
cd voxtype
cargo build --release
# Binary: target/release/voxtype
```

Or directly: `cargo install --git https://github.com/atheerium/voxtype`

### Option C: Package managers (when available)

Prebuilt packages are planned for the future. Until then, the installer is the
fastest path.

## Quick start

1. **Install** (see above).
2. **Set your API key** if you didn't during install:
   ```bash
   echo 'export GROQ_API_KEY="gsk_..."' >> ~/.bashrc
   ```
3. **Press <kbd>Ctrl</kbd>+<kbd>Space</kbd>**, speak, press <kbd>Ctrl</kbd>+<kbd>Space</kbd> again. Done.

Text is pasted into the focused app automatically, and is always on the
clipboard as a fallback (<kbd>Ctrl</kbd>+<kbd>V</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd>).

## Configuration

Configuration lives in `~/.config/voxtype/config.toml`:

```toml
groq_api_key = "gsk_..."           # Required: your Groq API key
language = "en"                    # ISO-639-1 code, optional
model = "whisper-large-v3-turbo"   # Any Groq model, optional
backend = "auto"                   # "auto", "x11", or "wayland"
audio_source = "default"           # PulseAudio/PipeWire source, optional
```

**API key resolution order:** `config.toml` → `GROQ_API_KEY` env var → shell rc files.

Find non-default audio sources with `pactl list sources short`.

## Manual hotkey setup (if you skipped the installer's)

<details>
<summary><b>Sway</b></summary>

```ini
# ~/.config/sway/config
exec_always /path/to/voxtype --daemon
bindsym Ctrl+space exec /path/to/voxtype
```

</details>

<details>
<summary><b>Hyprland</b></summary>

```ini
# ~/.config/hypr/hyprland.conf
exec-once = /path/to/voxtype --daemon
bind = CTRL, SPACE, exec, /path/to/voxtype
```

</details>

<details>
<summary><b>XFCE</b></summary>

```bash
xfconf-query -c xfce4-keyboard-shortcuts \
  -p "/commands/custom/<Primary>space" -s "/path/to/voxtype" --create -t string
```

</details>

<details>
<summary><b>GNOME</b></summary>

GNOME needs a custom shortcut (Settings → Keyboard → Keyboard Shortcuts → Add):
command `/path/to/voxtype`.

</details>

<details>
<summary><b>KDE Plasma</b></summary>

Add a custom shortcut in System Settings → Shortcuts → Add New → Global
Shortcut → Command/URL, command `/path/to/voxtype`.

</details>

## Troubleshooting

| Symptom | Fix |
|---|---|
| "Pasted N chars ✓" but nothing appears | The app's paste shortcut differs. Use <kbd>Ctrl</kbd>+<kbd>V</kbd> or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>V</kbd> manually; text is always on the clipboard |
| HTTP 401 | Invalid API key — check `~/.config/voxtype/config.toml` or `$GROQ_API_KEY` |
| HTTP 413 | Audio too large — speak for a shorter duration |
| HTTP 429 | Rate limited — wait and retry |
| "No audio recorded" | Microphone not selected; test: `ffmpeg -f pulse -i default -ac 1 -ar 16000 -t 3 /tmp/test.mp3` |
| "Audio file too small" | Recording too brief (< ~500 ms) |
| Wayland socket not found | Check `WAYLAND_DISPLAY`; restart your compositor session |
| Daemon not responding | `kill $(cat /tmp/voxtype.pid)` then run `voxtype --daemon` |
| See everything | `tail -f ~/.local/share/voxtype/daemon.log` |

## Privacy & security

- **No voxtype servers.** The daemon runs entirely on your machine. Audio goes
  directly to the transcription provider you configure (Groq by default).
- **Your API key stays local** (in `config.toml` or your shell env).
- **No telemetry, no analytics, no phone-home.** The only network call is the
  transcription request itself.
- **Transparent code.** MIT-licensed; every byte is auditable.
- **Careful process handling.** The daemon verifies PIDs against `/proc` before
  killing orphaned processes, and cleans up its own lock/audio/temp files.

## FAQ

**What is voxtype?**
voxtype is a free, open-source voice-to-text dictation tool for Linux. Press a
hotkey, speak, and your words are typed into the focused application via
Whisper transcription.

**How is voxtype different from Wispr Flow?**
Wispr Flow is a popular closed-source, subscription-based dictation app that
does not support Linux. voxtype is its free, open-source, MIT-licensed
alternative built for Linux (X11 and Wayland). It has no subscription, no
account, and no vendor cloud.

**Does voxtype work on Wayland?**
Yes. voxtype auto-detects Wayland and adapts its paste strategy per compositor
(Sway, Hyprland, KDE, GNOME), using `wtype` plus the clipboard.

**Does voxtype work on X11?**
Yes — XFCE, GNOME X11, i3, and any X11 window manager. It detects terminal vs
GUI windows and uses the correct paste shortcut.

**Which speech-to-text model does voxtype use?**
Groq's `whisper-large-v3-turbo` by default. Any Groq transcription model can be
set via the `model` config option.

**Do I need a GPU?**
No. Transcription runs on Groq's servers (their hardware is exceptionally
fast); your machine only records audio.

**Is voxtype really free?**
Yes. The software is MIT-licensed and costs nothing. The only cost is the
speech API usage from Groq, which has a generous free tier.

**Is my audio private?**
Your audio is sent only to the transcription provider you configure. There is
no voxtype cloud, no telemetry, and no analytics.

**Can I use another transcription provider?**
The code targets the OpenAI-compatible audio API, so swapping providers is a
small change — contributions welcome.

**How fast is transcription?**
Groq's Whisper endpoint typically returns results in under a couple of seconds
for short recordings; the whole loop is usually well under 5 seconds.

## Roadmap

- Voice editing commands (select, delete, replace) like Wispr Flow
- Offline transcription (whisper.cpp / local models)
- Provider abstraction (OpenAI-compatible API surface)
- System tray indicator
- Packaged releases (deb, rpm, AUR)

## Support & Donations

voxtype is free and always will be, made by [Atheerium](https://atheerium.com).
If it saves you time, consider supporting the work:

- ☕ [Buy me a coffee on Ko-fi](https://ko-fi.com/atheerium)
- ⭐ Star the repo — it helps others discover free dictation
- 🐛 Report bugs and open PRs

Every donation and star keeps open-source dictation alive and growing.

## Contributing

Contributions are welcome — code, docs, packaging, translations. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the workflow. Build with
`cargo build --release`, test with `cargo test`, keep `cargo clippy` and
`cargo fmt` clean.

## License

[MIT](LICENSE) © [Atheerium](https://atheerium.com)

---

<p align="center">
  Made with 💚 by <a href="https://atheerium.com">Atheerium</a> for Linux users who'd rather speak than type.<br>
  <a href="https://ko-fi.com/atheerium">☕ Support voxtype on Ko-fi</a> · <a href="https://atheerium.com">atheerium.com</a><br>
  Found this useful? <a href="https://github.com/atheerium/voxtype">⭐ Star voxtype on GitHub</a> — it helps more people discover free dictation.
</p>

---

> **GitHub repo description (paste into Settings → About):**
> Free, open-source voice-to-text dictation for Linux. Press Ctrl+Space, speak, and your words are typed into any app. A privacy-friendly, self-hostable Wispr Flow alternative for X11 and Wayland.
