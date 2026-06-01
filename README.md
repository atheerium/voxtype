# voxtype

Voice-to-text dictation for X11/XFCE (Linux Mint). Press a hotkey, speak, text appears.

## Architecture

```
Ctrl+Space → voxtype → SIGUSR1 → Daemon (background)
                                  ├── ffmpeg records mic → /tmp/voxtype.mp3
                                  └── Toggle off → Groq API → clipboard → paste
```

- **Daemon**: Starts silently at login. No recording on boot.
- **Toggle**: Ctrl+Space sends SIGUSR1 — press to record, press again to paste.
- **Smart paste**: Detects terminal vs GUI window, uses correct paste shortcut.
- **Dual clipboard**: xsel + xclip fallback chain for reliable clipboard setting.
- **Logging**: All errors written to `~/.local/share/voxtype/daemon.log`.

## Dependencies

```bash
sudo apt install ffmpeg xdotool xsel xclip
```

## Build

```bash
cargo build --release
# Binary: target/release/voxtype
```

## Configuration

```bash
export GROQ_API_KEY="gsk_your_key_here"
```

Or create `~/.config/voxtype/config.toml`:

```toml
groq_api_key = "gsk_your_key_here"
language = "en"                    # ISO-639-1, optional
```

## XFCE Hotkey

Bound to **Ctrl+Space**. To rebind:

```bash
xfconf-query -c xfce4-keyboard-shortcuts \
  -p "/commands/custom/<Primary>space" \
  -s "/path/to/voxtype"
```

## Autostart

Installed at `~/.config/autostart/voxtype.desktop` (with `--daemon` flag).
Starts silently — no recording on boot.

## Troubleshooting

```bash
# Check the log
cat ~/.local/share/voxtype/daemon.log

# Restart daemon
kill $(cat /tmp/voxtype.pid)
voxtype --daemon

# Test recording works
ffmpeg -f pulse -i default -ac 1 -ar 16000 -t 5 /tmp/test.mp3
```

## Binary Size

```
~2.4 MB (stripped, LTO, panic=abort)
```

## License

MIT
