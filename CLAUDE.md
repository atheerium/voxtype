# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**voxtype** is a free, open-source voice-to-text dictation tool for Linux —
marketed as the "Wispr Flow alternative." Press a hotkey, speak, and the transcribed text is automatically pasted into your active application.

Supports X11 (XFCE, GNOME X11, i3) and Wayland (Sway, Hyprland, KDE, GNOME) via auto-detection.

Published at https://github.com/atheerium/voxtype (public, `main` branch).
Brand: made by **Atheerium** — website https://atheerium.com, donations
https://ko-fi.com/atheerium. Keep the Atheerium credit in README, LICENSE,
install.sh, and package metadata.

## Build Commands

```bash
cargo build --release    # Build optimized binary
cargo build              # Debug build
cargo test               # Unit tests (11 tests: config parsing, env detection, helpers)
cargo clippy --all-targets -- -D warnings   # CI enforces zero warnings
cargo fmt --check        # CI checks formatting
```

The binary is output to `target/release/voxtype`.

## Release & Installer

- **README.md** is the marketing/SEO surface: one-command install at the top,
  Wispr Flow comparison, FAQ. Keep install commands in sync with `install.sh`.
- **install.sh** is the one-command installer (`curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash`).
  - Detects display server/compositor/distro, installs deps, downloads a
    prebuilt binary from GitHub Releases (assets named `voxtype-<triple>.tar.gz`
    with `sha256sums.txt`), falls back to `cargo install --git`.
  - Validate with `shellcheck install.sh`, `bash -n install.sh`, and
    `bash install.sh --dry-run`.
  - Never break `--dry-run` (it must not touch the system or the network).
- **Release flow**: pushing a `v*` tag runs `.github/workflows/release.yml`,
  which builds `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, and
  `aarch64-unknown-linux-musl` and publishes a GitHub Release.
  - **Bump `version` in `Cargo.toml` FIRST** (it feeds `voxtype --version`),
    commit, then tag `v<same-version>`.
  - The workflow ships `install.sh` as a release asset so the README one-liner
    (`releases/latest/download/install.sh`) is served by the fast object CDN.
- **CI** (`.github/workflows/ci.yml`): fmt, clippy `-D warnings`, tests, release build, installer lint.

## Architecture

```
Ctrl+Space → voxtype CLI → SIGUSR1 → Daemon (background)
                                     ├── ffmpeg records mic → /tmp/voxtype.mp3
                                     └── Toggle off → Groq API → clipboard → paste
```

- **[main.rs](src/main.rs)**: CLI entry point. Handles `--daemon` flag to start background daemon, or sends SIGUSR1 to toggle recording when invoked without flags.
- **[config.rs](src/config.rs)**: Configuration loading. Reads `~/.config/voxtype/config.toml`, falls back to `GROQ_API_KEY` env var, then parses shell RC files.
- **[dictation.rs](src/dictation.rs)**: Core daemon logic. Environment detection (X11/Wayland, compositor, audio system), recording via ffmpeg, Groq API transcription, text injection via clipboard + keyboard simulation.

### Key Design Patterns

- **Daemon**: Spawns as background process via `__daemon` internal argument. Writes PID to `/tmp/voxtype.pid`, uses lockfile at `/tmp/voxtype.lock` to track recording state.
- **Signal handlers first**: `run_daemon` registers SIGUSR1/SIGTERM/SIGINT handlers before any slow startup work. A toggle arriving mid-startup would otherwise hit SIGUSR1's default action and kill the daemon.
- **Single-instance guard**: the daemon exits quietly if the PID file is owned by another live process, closing the rapid-double-hotkey spawn race.
- **Startup self-healing**: a crashed daemon leaves an orphaned ffmpeg + stale lockfile; startup kills the orphan (PID verified against `/proc` so a recycled PID is never touched) and clears stale lock/audio state.
- **Toggle Guard**: `AtomicBool` prevents concurrent toggle operations — rapid hotkey presses are safely dropped.
- **Graceful Degradation**: Missing tools (xdotool, wtype, notify-send) are handled with fallbacks. Clipboard is always set even if keyboard paste fails. VS Code is detected (focused window on X11, `pgrep` on Wayland) and keyboard paste is skipped for it due to a shortcut conflict.
- **Environment Detection**: Auto-detects X11 vs Wayland, compositor (Sway/Hyprland/KDE/Gnome), and audio system (PulseAudio/PipeWire). `effective_env()` resolves the backend once — config `backend` override wins, else auto-detection — and is used consistently by startup logging, the display check, dependency validation, and text injection.
- **Log timestamps** are UTC with full date: `2026-08-05 21:38:43.817`. Written to `~/.local/share/voxtype/daemon.log`.

## Configuration

Create `~/.config/voxtype/config.toml`:

```toml
groq_api_key = "gsk_..."    # Required: get from console.groq.com
backend = "auto"            # "auto", "x11", or "wayland"
language = "en"             # ISO-639-1 code, optional
model = "whisper-large-v3-turbo"
audio_source = "default"    # PulseAudio source, optional
```

Key resolution order: config file → `GROQ_API_KEY` env var → shell RC files.

## Runtime Dependencies

X11: `ffmpeg`, `xdotool`, `xsel`, `xclip`
Wayland: `ffmpeg`, `wl-clipboard`, `wtype`

Optional: `notify-send` (desktop notifications), `pactl` (audio device detection)

## Hotkey Setup

- **XFCE**: `xfconf-query` to bind Ctrl+Space
- **Sway**: `bindsym Ctrl+space exec /path/to/voxtype` (NOT `--to-code`; plain
  bindsym is what works reliably — the `--to-code` form failed to fire on sway 1.9)
- **Hyprland**: `bind = CTRL, SPACE, exec, /path/to/voxtype`

Add to autostart with `voxtype --daemon`.