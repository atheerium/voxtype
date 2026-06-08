# voxtype

Voice-to-text dictation for Linux. Press a hotkey, speak, text appears in your terminal or GUI app.

Supports **X11** (XFCE, GNOME X11, i3, etc.) and **Wayland** (Sway, Hyprland, KDE, GNOME).

## Architecture

```
Ctrl+Space → voxtype → SIGUSR1 → Daemon (background)
                                  ├── ffmpeg records mic → /tmp/voxtype.mp3
                                  └── Toggle off → Groq API → clipboard → paste
```

- **Daemon**: Starts silently at login. No recording on boot.
- **Toggle**: Ctrl+Space sends SIGUSR1 — press to record, press again to paste.
- **Smart paste**: Detects terminal vs GUI on X11; compositor-aware shortcut selection on Wayland.
- **Clipboard**: Always set as fallback, even if keyboard paste fails.
- **Logging**: All events written to `~/.local/share/voxtype/daemon.log`.

## Environment Support Matrix

| Environment | Paste Method | Terminal Detection | Notes |
|---|---|---|---|
| **X11** (any WM) | `xdotool key` | `xprop WM_CLASS` + `ps` fallback | Requires xsel + xclip |
| **Wayland + Sway** | `wtype Ctrl+Shift+V` | Not supported | Try Ctrl+Shift+V first, then Ctrl+V |
| **Wayland + Hyprland** | `wtype Ctrl+Shift+V` | Not supported | Same as Sway |
| **Wayland + KDE** | `wtype Ctrl+Shift+V` | Not supported | Same |
| **Wayland + GNOME** | `wtype Ctrl+V` | Not supported | GNOME apps use Ctrl+V; try Ctrl+V first |
| **Wayland (other)** | `wtype Ctrl+Shift+V` | Not supported | Falls back to wl-copy clipboard |
| **No display server** | Clipboard only | N/A | Errors logged, manual paste required |
| **XWayland** | Wayland backend | N/A | Detects Wayland automatically |

On Wayland, text is **always copied to clipboard** (`wl-copy`) in addition to keyboard paste,
so you can manually paste with Ctrl+Shift+V or Ctrl+V if the automatic paste fails.

## Dependencies

All environments require **ffmpeg** for audio recording.

X11:
```bash
sudo apt install ffmpeg xdotool xsel xclip
```

Wayland:
```bash
sudo apt install ffmpeg wl-clipboard wtype
```

### Optional

| Tool | Needed For | If Missing |
|---|---|---|
| `notify-send` (libnotify) | Desktop notifications | Falls back to stderr |
| `pactl` (pulseaudio-utils) | Audio device detection | Graceful error with hint |

## Build

```bash
cargo build --release
# Binary: target/release/voxtype
```

## Configuration

Create `~/.config/voxtype/config.toml`:

```toml
groq_api_key = "gsk_your_key_here"    # Required: get one at console.groq.com
backend = "auto"                       # "auto", "x11", or "wayland"
language = "en"                        # ISO-639-1 code, optional
model = "whisper-large-v3-turbo"       # Groq model, optional
audio_source = "default"               # PulseAudio source name, optional
```

Or set the API key via environment:
```bash
export GROQ_API_KEY="gsk_your_key_here"
```

The key resolution chain is:
1. `groq_api_key` in config.toml
2. `GROQ_API_KEY` environment variable
3. Shell rc files (`.bashrc`, `.zshrc`, etc.)

### audio_source

Specify a non-default PulseAudio source name. Find available sources with:
```bash
pactl list sources short
```

Examples:
```toml
audio_source = "alsa_input.usb-Snowball_iCE-00.iec958-stereo"
audio_source = "default"     # system default (same as omitting the field)
```

## Setup

### X11 / XFCE

```bash
# Bind to Ctrl+Space:
xfconf-query -c xfce4-keyboard-shortcuts \
  -p "/commands/custom/<Primary>space" \
  -s "/path/to/voxtype"

# Autostart (add to ~/.config/autostart/voxtype.desktop):
[Desktop Entry]
Type=Application
Name=voxtype
Exec=/path/to/voxtype --daemon
```

### Wayland / Sway

Add to `~/.config/sway/config`:
```
exec /path/to/voxtype --daemon
bindsym --to-code Ctrl+space exec /path/to/voxtype
```

Replace `/path/to/voxtype` with the full path (e.g. `$HOME/dev/ethreal-voice/target/release/voxtype`).

### Wayland / Hyprland

Add to `~/.config/hypr/hyprland.conf`:
```
exec-once = /path/to/voxtype --daemon
bind = CTRL, SPACE, exec, /path/to/voxtype
```

## Edge Cases & Failure Modes

### Concurrent toggle protection
If you press Ctrl+Space twice rapidly, the second signal is safely dropped
while the first transcription is in progress. Prevents duplicate ffmpeg
processes and concurrent API calls.

### Missing display server
If neither `DISPLAY` nor `WAYLAND_DISPLAY` is set, the daemon logs a warning
and text injection will fail with a clear error message.

### Missing dependencies
Each backend validates its required tools at startup:
- **X11**: xdotool, xsel, xclip
- **Wayland**: wl-copy, wtype

Missing tools are logged and the `--daemon` startup message tells you what
to install. Backend operations fail with actionable hints.

### Clipboard-only fallback
If the paste tool (xdotool/wtype) is missing or fails:
- Text is still set on the clipboard (via xsel/xclip or wl-copy)
- Notification tells you to paste manually
- Daemon log records the event

### Audio system detection
Detects PulseAudio, PipeWire (via pipewire-pulse compat), or neither.
If no audio system is running, recording fails with clear next steps.
Configurable via `audio_source` for non-default microphones.

### Groq API errors
- **HTTP 401**: Invalid API key — check config.toml or env var
- **HTTP 413**: Audio too large — speak for a shorter duration
- **HTTP 429**: Rate limited — wait and retry
- **Timeout**: Network issue — check internet connection
- **Empty response**: No speech detected — speak more clearly

### SIGTERM / SIGINT cleanup
On shutdown, the daemon:
1. Kills the ffmpeg recording process
2. Removes the lock file, PID file, and temp audio file
3. Writes a final log entry

### Stale PID file
If the daemon crashes, `main.rs` uses `kill -0` to verify the PID is alive
before sending signals. A new daemon writes a fresh PID file.

### notify-send not available
Desktop notifications fall back to stderr output. You'll see messages like
`[voxtype] Recording...` if you launched the toggle from a terminal.

### Recording too short
If you toggle off within ~500ms of starting, the audio file may be empty
or too small. The daemon checks for this and reports it clearly.

### Audio file too large
Groq has a ~25 MB upload limit. The daemon checks file size before upload
and warns if the recording is too long or high-bitrate.

## Troubleshooting

```bash
# Check the log
cat ~/.local/share/voxtype/daemon.log

# Restart daemon
kill $(cat /tmp/voxtype.pid)
voxtype --daemon

# Test recording works
ffmpeg -f pulse -i default -ac 1 -ar 16000 -t 5 /tmp/test.mp3

# List audio sources (to set audio_source in config)
pactl list sources short

# Check Wayland socket exists
ls -la $WAYLAND_DISPLAY

# Verify X11 display
echo $DISPLAY
```

### "Pasted N chars ✓" but nothing appears

This usually means the keyboard paste shortcut doesn't match your app:

| App Type | Try Pasting With |
|---|---|
| Terminal (Alacritty, foot, kitty, etc.) | **Ctrl+Shift+V** |
| GUI app (browser, editor) | **Ctrl+V** |
| GNOME apps | **Ctrl+V** |

Text is always copied to the clipboard, so manual paste works.
If automatic paste consistently fails for your setup, check `backend` config
or file an issue.

## Binary Size

```
~2.4 MB (stripped, LTO, panic=abort)
```

## License

MIT
