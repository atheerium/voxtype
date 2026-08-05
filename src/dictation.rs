use anyhow::{Context, Result};
use reqwest::multipart;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::process::Command as TokioCommand;
use tokio::signal::unix::{signal, SignalKind};

use crate::config::Config;

// ── Environment detection ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DesktopEnv {
    X11,
    Wayland,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WaylandCompositor {
    Sway,
    Hyprland,
    KDE,
    Gnome,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AudioSystem {
    PulseAudio,
    PipeWire,
    None,
}

pub fn detect_env() -> DesktopEnv {
    // WAYLAND_DISPLAY being set is the canonical Wayland check.
    // XDG_SESSION_TYPE is a fallback for compositors that don't set WAYLAND_DISPLAY.
    // On XWayland, both DISPLAY and WAYLAND_DISPLAY are set — Wayland wins.
    if std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
    {
        DesktopEnv::Wayland
    } else if std::env::var("DISPLAY").is_ok() {
        DesktopEnv::X11
    } else {
        // No display server at all — prefer Wayland as safer fallback
        DesktopEnv::Wayland
    }
}

pub fn detect_wayland_compositor() -> WaylandCompositor {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    if desktop.contains("sway") {
        WaylandCompositor::Sway
    } else if desktop.contains("hyprland") {
        WaylandCompositor::Hyprland
    } else if desktop.contains("kde") || desktop.contains("plasma") {
        WaylandCompositor::KDE
    } else if desktop.contains("gnome") || desktop.contains("mutter") {
        WaylandCompositor::Gnome
    } else {
        WaylandCompositor::Other
    }
}

pub fn detect_audio_system() -> AudioSystem {
    // PipeWire >= 0.3 provides a pulse-compatible socket at the same path
    let pulse_info = Command::new("pactl")
        .args(["info"])
        .output();
    if let Ok(out) = &pulse_info {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains("PipeWire") || stdout.contains("pipewire") {
            return AudioSystem::PipeWire;
        }
        if out.status.success() {
            return AudioSystem::PulseAudio;
        }
    }

    // Check for pure PipeWire (no pulse compat layer)
    if Command::new("pipewire").arg("--version").output().is_ok() {
        return AudioSystem::PipeWire;
    }

    AudioSystem::None
}

pub fn deps_install_hint(env: DesktopEnv) -> &'static str {
    match env {
        DesktopEnv::X11 => "sudo apt install ffmpeg xdotool xsel xclip",
        DesktopEnv::Wayland => "sudo apt install ffmpeg wl-clipboard wtype",
    }
}

/// Validate that we're actually connected to a display server before
/// attempting backend-specific operations.
pub fn check_display_env(env: DesktopEnv) -> Result<()> {
    match env {
        DesktopEnv::X11 => {
            let display = std::env::var("DISPLAY")
                .map_err(|_| anyhow::anyhow!(
                    "DISPLAY is not set. voxtype needs an X11 display.\n\
                     Make sure you're running this from within an X session.\n\
                     If using Wayland, set backend = \"wayland\" in config.toml."
                ))?;
            if display.is_empty() {
                anyhow::bail!("DISPLAY is set but empty. Check your X11 session.");
            }
        }
        DesktopEnv::Wayland => {
            let wl = std::env::var("WAYLAND_DISPLAY")
                .map_err(|_| anyhow::anyhow!(
                    "WAYLAND_DISPLAY is not set. voxtype needs a Wayland compositor.\n\
                     Make sure you're running this from within a Wayland session.\n\
                     If using X11, set backend = \"x11\" in config.toml."
                ))?;
            if wl.is_empty() {
                anyhow::bail!("WAYLAND_DISPLAY is set but empty. Check your Wayland session.");
            }
            // WAYLAND_DISPLAY may be an absolute path or a name relative to
            // XDG_RUNTIME_DIR. Never hardcode /run/user/<uid>: the daemon
            // can run under any UID.
            let socket = if wl.starts_with('/') {
                PathBuf::from(&wl)
            } else {
                let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                    .ok()
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
                    .or_else(dirs::runtime_dir)
                    .ok_or_else(|| anyhow::anyhow!(
                        "Cannot determine Wayland runtime directory (XDG_RUNTIME_DIR is not set)"
                    ))?;
                runtime_dir.join(&wl)
            };
            if !socket.exists() {
                anyhow::bail!("Wayland socket {} does not exist. Check your compositor.", socket.display());
            }
        }
    }
    Ok(())
}

// ── File paths ────────────────────────────────────────────────────

const PIDFILE: &str = "/tmp/voxtype.pid";
const LOCKFILE: &str = "/tmp/voxtype.lock";
const AUDIO_FILE: &str = "/tmp/voxtype.mp3";

/// True if a live process owns the daemon PID file.
pub fn daemon_running() -> bool {
    daemon_pid().map(process_alive).unwrap_or(false)
}

/// PID stored in the daemon PID file, if any.
pub fn daemon_pid() -> Option<u32> {
    fs::read_to_string(PIDFILE).ok().and_then(|c| c.trim().parse::<u32>().ok())
}

fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn log_path() -> Result<PathBuf> {
    let data_dir = dirs::data_dir().context("Cannot determine data directory")?;
    let dir = data_dir.join("voxtype");
    let _ = fs::create_dir_all(&dir);
    Ok(dir.join("daemon.log"))
}

fn write_log(msg: &str) {
    if let Ok(path) = log_path() {
        let line = format!("[{}] {}: {}\n", chrono_now(), std::process::id(), msg);
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (y, mo, da) = civil_from_days((secs / 86400) as i64);
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        y, mo, da, h, m, s, d.subsec_millis()
    )
}

/// Convert days since 1970-01-01 (Unix epoch) to a (year, month, day)
/// civil date. UTC-based, good enough for a log timestamp without pulling
/// in a chrono dependency. Based on Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ── Concurrent toggle guard ────────────────────────────────────────

/// Prevents re-entrant toggle() while a previous SIGUSR1 is in progress.
/// Without this, rapid double-presses can corrupt state (two ffmpeg processes,
/// concurrent API calls, etc.).
static TOGGLE_BUSY: AtomicBool = AtomicBool::new(false);

struct ToggleGuard;

impl ToggleGuard {
    fn try_acquire() -> Option<Self> {
        if TOGGLE_BUSY.swap(true, Ordering::AcqRel) {
            None // already busy
        } else {
            Some(ToggleGuard)
        }
    }
}

impl Drop for ToggleGuard {
    fn drop(&mut self) {
        TOGGLE_BUSY.store(false, Ordering::Release);
    }
}

// ── State ─────────────────────────────────────────────────────────

fn is_recording() -> bool {
    Path::new(LOCKFILE).exists()
}

fn set_recording(ffmpeg_pid: u32) -> Result<()> {
    fs::write(LOCKFILE, ffmpeg_pid.to_string()).context("Failed to write lockfile")?;
    Ok(())
}

fn clear_recording() {
    let _ = fs::remove_file(LOCKFILE);
}

fn read_lockfile_pid() -> Option<u32> {
    fs::read_to_string(LOCKFILE).ok().and_then(|c| c.trim().parse::<u32>().ok())
}

fn kill_ffmpeg() {
    if let Some(pid) = read_lockfile_pid() {
        let _ = Command::new("kill").arg(pid.to_string()).output();
    }
}

// ── Tool validation ───────────────────────────────────────────────

fn require_tool(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn validate_deps() -> Vec<String> {
    let mut missing = Vec::new();
    if !require_tool("ffmpeg") {
        missing.push("ffmpeg".to_string());
    }
    let env = detect_env();
    match env {
        DesktopEnv::X11 => {
            for tool in &["xdotool", "xsel", "xclip"] {
                if !require_tool(tool) {
                    missing.push(tool.to_string());
                }
            }
        }
        DesktopEnv::Wayland => {
            for tool in &["wl-copy", "wtype"] {
                if !require_tool(tool) {
                    missing.push(tool.to_string());
                }
            }
        }
    }
    missing
}

// ── Notification ──────────────────────────────────────────────────

/// Send a desktop notification if notify-send is available.
/// Falls back to stderr (useful when running from terminal or
/// when no notification daemon is installed).
fn notify(summary: &str, body: &str) {
    let sent = Command::new("notify-send")
        .args(["-a", "voxtype", summary, body])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !sent {
        // Fallback: write to stderr so users launching from terminal see it
        eprintln!("[voxtype] {}: {}", summary, body);
    }
}

// ── Daemon ────────────────────────────────────────────────────────

pub async fn run_daemon() -> Result<()> {
    // Single-instance guard: if another live daemon owns the PID file,
    // exit quietly. This closes the race where two rapid toggles both
    // decided to spawn a daemon at the same instant.
    if daemon_pid().is_some_and(|pid| pid != std::process::id() && process_alive(pid)) {
        return Ok(());
    }

    // Write PID file
    fs::write(PIDFILE, std::process::id().to_string())
        .context("Failed to write PID file")?;

    // A previous daemon may have died mid-recording (crash or SIGKILL),
    // leaving an orphaned ffmpeg and a stale lockfile behind. Clean these
    // up so a fresh recording can't conflict with leftover state.
    cleanup_stale_state();

    // Register signal handlers BEFORE any slow startup work (env checks,
    // dependency validation, logging). A toggle (SIGUSR1) arriving before
    // the handler is installed would terminate the daemon, because
    // SIGUSR1's default action is to kill the process.
    let mut usr1 = signal(SignalKind::user_defined1())
        .context("Failed to setup SIGUSR1 handler")?;
    let mut term = signal(SignalKind::terminate())
        .context("Failed to setup SIGTERM handler")?;
    let mut int = signal(SignalKind::interrupt())
        .context("Failed to setup SIGINT handler")?;

    // Validate environment and dependencies
    let env = detect_env();
    let compositor = match env {
        DesktopEnv::Wayland => Some(detect_wayland_compositor()),
        DesktopEnv::X11 => None,
    };
    let audio = detect_audio_system();

    // Log startup info
    match env {
        DesktopEnv::X11 => {
            write_log(&format!(
                "Daemon started (X11). DISPLAY={:?}, XAUTHORITY={:?}",
                std::env::var("DISPLAY").unwrap_or_default(),
                std::env::var("XAUTHORITY").unwrap_or_default()
            ));
        }
        DesktopEnv::Wayland => {
            let comp = compositor.map(|c| format!("{:?}", c)).unwrap_or_default();
            write_log(&format!(
                "Daemon started (Wayland, {}). WAYLAND_DISPLAY={:?}",
                comp,
                std::env::var("WAYLAND_DISPLAY").unwrap_or_default(),
            ));
        }
    }

    if let AudioSystem::None = audio {
        write_log("WARNING: No audio system detected (pactl/pipewire not found). Recording will fail.");
        eprintln!("voxtype WARNING: No audio system detected. Install pulseaudio-utils or pipewire.");
    } else {
        write_log(&format!("Audio system: {:?}", audio));
    }

    // Validate runtime dependencies
    let missing = validate_deps();
    if !missing.is_empty() {
        let msg = format!(
            "Missing runtime dependencies: {}. Install with: {}",
            missing.join(", "),
            deps_install_hint(env)
        );
        write_log(&msg);
        eprintln!("voxtype: {}", msg);
    }

    // Log display env is healthy
    let env_check = check_display_env(env);
    if let Err(e) = env_check {
        write_log(&format!("WARNING: {}", e));
        eprintln!("voxtype WARNING: {}", e);
    }

    // Signal handlers were registered before startup checks; see above.

    loop {
        tokio::select! {
            _ = usr1.recv() => {
                write_log("SIGUSR1 received - toggling");
                toggle().await;
            }
            _ = term.recv() => {
                write_log("SIGTERM received - shutting down");
                cleanup();
                break;
            }
            _ = int.recv() => {
                write_log("SIGINT received - shutting down");
                cleanup();
                break;
            }
        }
    }

    Ok(())
}

fn cleanup() {
    kill_ffmpeg();
    clear_recording();
    let _ = fs::remove_file(PIDFILE);
    let _ = fs::remove_file(AUDIO_FILE);
}

/// On startup, a previous daemon may have died mid-recording, leaving an
/// orphaned ffmpeg writing to AUDIO_FILE and a stale LOCKFILE. Kill the
/// orphan (only when it really is an ffmpeg process, to avoid killing an
/// unrelated process that reused the PID) and remove the stale state so a
/// fresh recording can't conflict with it.
fn cleanup_stale_state() {
    if let Some(pid) = read_lockfile_pid() {
        if is_process_named(pid, "ffmpeg") {
            let _ = Command::new("kill").arg(pid.to_string()).output();
            write_log(&format!(
                "Killed orphaned ffmpeg (pid {}) left by a previous session",
                pid
            ));
        } else {
            write_log(&format!(
                "Stale lockfile references pid {} (not ffmpeg); ignoring",
                pid
            ));
        }
    }
    clear_recording();
    let _ = fs::remove_file(AUDIO_FILE);
}

fn is_process_named(pid: u32, name: &str) -> bool {
    // /proc is Linux-specific; `ps` is the portable fallback.
    if let Ok(comm) = fs::read_to_string(format!("/proc/{}/comm", pid)) {
        return comm.trim() == name;
    }
    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == name)
        .unwrap_or(false)
}

// ── Toggle ────────────────────────────────────────────────────────

async fn toggle() {
    // Guard: drop duplicate toggle signals while one is in progress
    let _guard = match ToggleGuard::try_acquire() {
        Some(g) => g,
        None => {
            write_log("SIGUSR1 dropped — toggle already in progress");
            return;
        }
    };

    if is_recording() {
        notify("voxtype", "Transcribing...");
        match stop_and_transcribe().await {
            Ok(text) => {
                write_log(&format!("Transcribed and injected: {} chars", text.len()));
                let msg = format!("Pasted {} chars ✓", text.len());
                notify("voxtype", &msg);
            }
            Err(e) => {
                let msg = format!("{}", e);
                write_log(&format!("Transcription/paste failed: {}", msg));
                notify("voxtype", &msg);
            }
        }
    } else {
        match start_recording() {
            Ok(()) => {
                write_log("Recording started");
                notify("voxtype", "Recording...");
            }
            Err(e) => {
                let msg = format!("Recording failed: {}", e);
                write_log(&msg);
                notify("voxtype", &msg);
            }
        }
    }
}

// ── Recording ─────────────────────────────────────────────────────

fn start_recording() -> Result<()> {
    kill_ffmpeg();
    let _ = fs::remove_file(AUDIO_FILE);

    // Check ffmpeg availability
    if !require_tool("ffmpeg") {
        anyhow::bail!(
            "ffmpeg not found. Install: sudo apt install ffmpeg"
        );
    }

    // Check audio system
    let audio = detect_audio_system();
    match audio {
        AudioSystem::PulseAudio | AudioSystem::PipeWire => {
            // pactl info works for both pulseaudio and pipewire-pulse
            let pa_check = Command::new("pactl")
                .args(["info"])
                .output();
            if let Err(e) = pa_check {
                anyhow::bail!(
                    "pactl info failed ({}). Is PulseAudio/PipeWire running?\n\
                     Try: pulseaudio --start  or  systemctl --user start pipewire",
                    e
                );
            }
        }
        AudioSystem::None => {
            anyhow::bail!(
                "No audio system detected. Install one of:\n\
                 - sudo apt install pulseaudio pulseaudio-utils\n\
                 - sudo apt install pipewire pipewire-pulse wireplumber"
            );
        }
    }

    // Read config for optional audio source override
    let config = Config::load().ok();
    let audio_source = config
        .and_then(|c| c.audio_source.clone())
        .unwrap_or_else(|| "default".to_string());

    let child = TokioCommand::new("ffmpeg")
        .args([
            "-y",
            "-f", "pulse",
            "-i", &audio_source,
            "-ac", "1",
            "-ar", "16000",
            "-b:a", "64k",
            "-loglevel", "error",
            AUDIO_FILE,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to start ffmpeg recording (check microphone)")?;

    let pid = child.id().unwrap_or(0);
    set_recording(pid)?;

    tokio::spawn(async move {
        let result = child.wait_with_output().await;
        if let Err(e) = result {
            write_log(&format!("ffmpeg exited with error: {}", e));
        } else {
            write_log("ffmpeg exited");
        }
    });

    Ok(())
}

// ── Transcription ─────────────────────────────────────────────────

async fn stop_and_transcribe() -> Result<String> {
    kill_ffmpeg();

    // Give ffmpeg time to finalize the file
    tokio::time::sleep(Duration::from_millis(400)).await;

    clear_recording();

    let meta = fs::metadata(AUDIO_FILE)
        .context("No audio recorded. Recording may have been too brief.")?;

    if meta.len() < 1024 {
        let _ = fs::remove_file(AUDIO_FILE);
        anyhow::bail!(
            "Audio file too small ({} bytes). Check microphone:\n\
             - Is your mic plugged in and selected as default?\n\
             - Test: ffmpeg -f pulse -i default -ac 1 -ar 16000 -t 3 /tmp/test.mp3",
            meta.len()
        );
    }

    let config = Config::load()?;
    let api_key = config.groq_api_key()?;

    let text = transcribe(&api_key, &config).await?;

    if text.trim().is_empty() {
        let _ = fs::remove_file(AUDIO_FILE);
        anyhow::bail!("Transcription returned empty (no speech detected)");
    }

    // Check that we can inject before removing the audio file
    inject_text(&text)?;

    let _ = fs::remove_file(AUDIO_FILE);
    Ok(text)
}

async fn transcribe(api_key: &str, config: &Config) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create HTTP client")?;

    let audio_bytes = fs::read(AUDIO_FILE)
        .context("Failed to read audio file for upload")?;

    // Groq has a ~25MB file limit, check early
    if audio_too_large(audio_bytes.len()) {
        anyhow::bail!(
            "Audio file too large ({} MB). Maximum is ~20 MB.\n\
             Speak for a shorter duration or reduce bitrate.",
            audio_bytes.len() / 1_000_000
        );
    }

    let file_part = multipart::Part::bytes(audio_bytes)
        .file_name("recording.mp3")
        .mime_str("audio/mpeg")
        .context("Invalid MIME type")?;

    let mut form = multipart::Form::new()
        .part("file", file_part)
        .text("model", config.model().to_string())
        .text("response_format", "text");

    if let Some(lang) = config.language() {
        form = form.text("language", lang.to_string());
    }

    let response = client
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .context("Failed to reach Groq API (check network/internet)")?;

    let status = response.status();
    let body = response.text().await
        .context("Failed to read API response")?;

    if !status.is_success() {
        let hint = match status.as_u16() {
            401 => "\nHint: Your GROQ_API_KEY is invalid. Check ~/.config/voxtype/config.toml or your shell rc file.",
            402 | 429 => "\nHint: Groq rate limit exceeded. Wait a moment and try again.",
            413 => "\nHint: Audio file too large for Groq's API limit.",
            _ => "",
        };
        anyhow::bail!("Groq API error (HTTP {}): {}{}", status, body, hint);
    }

    // response_format=text returns raw text
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(text) = json["text"].as_str() {
            return Ok(text.to_string());
        }
    }

    let trimmed = body.trim();
    if !trimmed.is_empty() {
        return Ok(trimmed.to_string());
    }

    anyhow::bail!("Empty response from Groq API (unexpected)");
}

// ── Text Injection ────────────────────────────────────────────

fn inject_text(text: &str) -> Result<()> {
    let config = Config::load()?;
    let env = match config.backend.as_deref() {
        Some("x11") => DesktopEnv::X11,
        Some("wayland") => DesktopEnv::Wayland,
        _ => detect_env(),
    };

    // Validate display env before attempting injection
    check_display_env(env)?;

    match env {
        DesktopEnv::X11 => inject_text_x11(text),
        DesktopEnv::Wayland => inject_text_wayland(text),
    }
}

fn inject_text_x11(text: &str) -> Result<()> {
    // 1. Set clipboard via xsel (primary)
    let xsel_ok = set_clipboard_xsel(text).is_ok();

    // 2. Fallback to xclip if xsel fails
    if !xsel_ok {
        set_clipboard_xclip(text)
            .context("Both xsel and xclip failed to set clipboard. Install: sudo apt install xsel xclip")?;
    }

    // 3. Wait for clipboard propagation
    std::thread::sleep(Duration::from_millis(100));

    // 4. VS Code has keyboard shortcut conflicts with xdotool paste
    if is_vscode_window_x11() {
        write_log("VS Code detected — skipping keyboard paste (clipboard set). Manual paste: Ctrl+V");
        return Ok(());
    }

    // 5. Detect active window type
    let is_term = is_terminal_window();

    write_log(&format!(
        "Injecting {} chars into {} window (X11)",
        text.len(),
        if is_term { "terminal" } else { "GUI" }
    ));

    // 6. Simulate paste via xdotool
    if !require_tool("xdotool") {
        // Clipboard is set, just warn
        write_log("xdotool not found. Text copied to clipboard (manual paste: Ctrl+V / Ctrl+Shift+V). Install: sudo apt install xdotool");
        return Ok(());
    }

    let shortcut = if is_term { "ctrl+shift+v" } else { "ctrl+v" };
    Command::new("xdotool")
        .args(["key", shortcut])
        .output()
        .with_context(|| format!("xdotool key {} failed. Is DISPLAY set correctly?", shortcut))?;

    Ok(())
}

fn inject_text_wayland(text: &str) -> Result<()> {
    // 1. Set clipboard (wl-copy)
    set_clipboard_wl_copy(text)
        .context("Failed to set Wayland clipboard. Install: sudo apt install wl-clipboard")?;

    std::thread::sleep(Duration::from_millis(100));

    // 2. If wtype not available, clipboard-only mode is fine
    if !require_tool("wtype") {
        write_log(&format!(
            "wtype not found. Copied {} chars to clipboard (manual paste: Ctrl+Shift+V / Ctrl+V). Install: sudo apt install wtype",
            text.len()
        ));
        return Ok(());
    }

    // 3. Skip wtype if VS Code is running (shortcut conflicts on Wayland)
    if is_vscode_running() {
        write_log("VS Code running — skipping wtype paste (clipboard set). Manual paste: Ctrl+V / Ctrl+Shift+V");
        return Ok(());
    }

    // 4. Determine paste shortcuts based on compositor
    let compositor = detect_wayland_compositor();

    write_log(&format!(
        "Pasting {} chars via wtype on {:?}",
        text.len(),
        compositor
    ));

    // Key combinations to try, ordered by likelihood for the detected compositor.
    // Terminal paste   = Ctrl+Shift+V
    // GUI app paste    = Ctrl+V
    let paste_keys = paste_keys_for(compositor);

    let mut any_success = false;
    for keys in paste_keys {
        let status = Command::new("wtype")
            .args(*keys)
            .output()
            .context("wtype failed to execute")?;

        if status.status.success() {
            std::thread::sleep(Duration::from_millis(50));
            any_success = true;
            break;
        } else {
            let stderr = String::from_utf8_lossy(&status.stderr);
            write_log(&format!("wtype attempt failed: {}", stderr.trim()));
        }
    }

    if !any_success {
        write_log("wtype paste failed — clipboard is set (manual paste: Ctrl+Shift+V / Ctrl+V)");
    }

    Ok(())
}

fn set_clipboard_wl_copy(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "wl-copy not found: {}. Install: sudo apt install wl-clipboard",
                e
            )
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())
            .context("Failed to write to wl-copy stdin")?;
    }

    child.wait().context("wl-copy failed")?;
    Ok(())
}

fn set_clipboard_xsel(text: &str) -> Result<()> {
    let mut child = Command::new("xsel")
        .args(["--clipboard", "--input"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("xsel not found: {}. Install: sudo apt install xsel", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())
            .context("Failed to write to xsel stdin")?;
    }

    child.wait().context("xsel (clipboard) failed")?;
    Ok(())
}

fn set_clipboard_xclip(text: &str) -> Result<()> {
    let mut child = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("xclip not found: {}. Install: sudo apt install xclip", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())
            .context("Failed to write to xclip stdin")?;
    }

    child.wait().context("xclip (clipboard) failed")?;
    Ok(())
}

// ── Window classification helpers ──────────────────────────────

/// Extract the class from an `xprop -id <win> WM_CLASS` output line,
/// e.g. `WM_CLASS(STRING) = "alacritty", "Alacritty"` -> `alacritty`.
fn parse_wm_class(output: &str) -> Option<String> {
    let class = output.rsplit('"').nth(1)?.trim().to_lowercase();
    if class.is_empty() { None } else { Some(class) }
}

fn is_known_terminal(name: &str) -> bool {
    const KNOWN_TERMINALS: &[&str] = &[
        "alacritty", "xfce4-terminal", "xfterminal", "gnome-terminal",
        "gnome-terminal-server", "konsole", "xterm", "uxterm",
        "urxvt", "urxvtc", "terminator", "tilix", "kitty",
        "wezterm", "st", "st-256color", "rxvt", "foot", "footclient",
        "guake", "mate-terminal", "lxterminal", "cool-retro-term",
        "deepin-terminal", "sakura", "termite", "ghostty",
        "blackbox", "contour", "tabby", "warp-terminal",
    ];
    KNOWN_TERMINALS.contains(&name)
}

/// Wayland paste key sequences to try, ordered by likelihood for the
/// detected compositor. Terminal paste = Ctrl+Shift+V, GUI paste = Ctrl+V.
fn paste_keys_for(compositor: WaylandCompositor) -> &'static [&'static [&'static str]] {
    match compositor {
        // GNOME: most apps use Ctrl+V; terminals need Ctrl+Shift+V
        WaylandCompositor::Gnome => &[
            &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
            &["-M", "ctrl", "-M", "shift", "-k", "v", "-m", "ctrl", "-m", "shift"],
        ],
        // Sway/Hyprland/KDE: terminals common, try Ctrl+Shift+V first
        _ => &[
            &["-M", "ctrl", "-M", "shift", "-k", "v", "-m", "ctrl", "-m", "shift"],
            &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
        ],
    }
}

/// Groq has a ~25 MB upload limit; reject anything approaching it early.
fn audio_too_large(len: usize) -> bool {
    len > 20_000_000
}

fn is_terminal_window() -> bool {
    // Step 1: Get active window ID via xdotool
    let winid = Command::new("xdotool")
        .args(["getactivewindow"])
        .output();

    let winid = match winid {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => {
            write_log(&format!("xdotool getactivewindow failed: {}", e));
            return false;
        }
    };

    if winid.is_empty() {
        write_log("xdotool returned empty window ID (no focused window?)");
        return false;
    }

    // Step 2: Get WM_CLASS via xprop (reliable on all X11)
    let xprop_out = Command::new("xprop")
        .args(["-id", &winid, "WM_CLASS"])
        .output();

    if let Ok(out) = xprop_out {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !stdout.trim().is_empty() {
            // WM_CLASS(STRING) = "instance", "class"
            match parse_wm_class(&stdout) {
                Some(class) => {
                    let is_term = is_known_terminal(&class);
                    write_log(&format!(
                        "Window WM_CLASS: '{}' -> {}",
                        class,
                        if is_term { "terminal" } else { "not terminal" }
                    ));
                    if is_term { return true; }
                }
                None => {
                    write_log(&format!("Could not parse WM_CLASS from: {}", stdout.trim()));
                }
            }
        } else {
            write_log("xprop returned empty output for WM_CLASS");
        }
    } else {
        write_log("xprop not found, falling back to PID detection");
    }

    // Step 3: Fallback to PID-based detection
    if let Ok(out) = Command::new("xdotool")
        .args(["getactivewindow", "getwindowpid"])
        .output()
    {
        let pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Ok(pid_num) = pid.parse::<u32>() {
            if let Ok(ps_out) = Command::new("ps")
                .args(["-p", &pid_num.to_string(), "-o", "comm="])
                .output()
            {
                let proc_name = String::from_utf8_lossy(&ps_out.stdout).trim().to_lowercase();
                let is_term = is_known_terminal(&proc_name);
                write_log(&format!(
                    "PID {} process: '{}' -> {}",
                    pid_num, proc_name,
                    if is_term { "terminal" } else { "not terminal" }
                ));
                return is_term;
            }
        }
    }

    false
}

// ── VS Code detection ──────────────────────────────────────────

/// Check if the active X11 window is VS Code, which has keyboard shortcut
/// conflicts with xdotool paste (opens chat tab instead of pasting).
fn is_vscode_window_x11() -> bool {
    let winid = match Command::new("xdotool").args(["getactivewindow"]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => {
            write_log(&format!("xdotool getactivewindow failed: {}", e));
            return false;
        }
    };

    if winid.is_empty() {
        return false;
    }

    match Command::new("xprop").args(["-id", &winid, "WM_CLASS"]).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if let Some(class) = parse_wm_class(&stdout) {
                let known_ides: &[&str] = &["code", "code-oss", "vscode", "vscodium"];
                let is_ide = known_ides.contains(&class.as_str());
                if is_ide {
                    write_log(&format!(
                        "Window WM_CLASS: '{}' -> IDE, skipping keyboard paste", class
                    ));
                }
                return is_ide;
            }
            false
        }
        Err(e) => {
            write_log(&format!("xprop failed: {}", e));
            false
        }
    }
}

/// Check if VS Code is running via process name. Used on Wayland where
/// we can't detect the focused window directly.
fn is_vscode_running() -> bool {
    for name in &["code", "code-oss", "codium"] {
        if Command::new("pgrep")
            .args(["-x", name])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wm_class_parsing() {
        assert_eq!(
            parse_wm_class(r#"WM_CLASS(STRING) = "alacritty", "Alacritty""#),
            Some("alacritty".to_string())
        );
        assert_eq!(parse_wm_class(""), None);
        assert_eq!(parse_wm_class("WM_CLASS(STRING) = "), None);
        assert_eq!(parse_wm_class(r#"WM_CLASS(STRING) = "code", "Code""#), Some("code".to_string()));
    }

    #[test]
    fn terminal_classification() {
        assert!(is_known_terminal("alacritty"));
        assert!(is_known_terminal("ghostty"));
        assert!(is_known_terminal("st"));
        assert!(is_known_terminal("xterm"));
        assert!(!is_known_terminal("firefox"));
        assert!(!is_known_terminal(""));
    }

    #[test]
    fn audio_size_limit() {
        assert!(!audio_too_large(1024));
        assert!(!audio_too_large(20_000_000));
        assert!(audio_too_large(20_000_001));
    }

    #[test]
    fn paste_key_selection() {
        let gnome = paste_keys_for(WaylandCompositor::Gnome);
        // GNOME: plain Ctrl+V first
        assert_eq!(gnome[0], &["-M", "ctrl", "-k", "v", "-m", "ctrl"][..]);
        assert_eq!(gnome[1][3], "shift");

        let sway = paste_keys_for(WaylandCompositor::Sway);
        // Sway: Ctrl+Shift+V first
        assert_eq!(sway[0][3], "shift");
        assert_eq!(sway[1], &["-M", "ctrl", "-k", "v", "-m", "ctrl"][..]);

        assert_eq!(paste_keys_for(WaylandCompositor::Other)[0][3], "shift");
    }

    #[test]
    fn civil_date_conversion() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-08-05: days since epoch = 20670
        assert_eq!(civil_from_days(20670), (2026, 8, 5));
        // Leap year boundary: 2000-02-29 = epoch day 11016
        assert_eq!(civil_from_days(11016), (2000, 2, 29));
        // 1969-12-31 = epoch day -1 (algorithm handles negatives)
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn log_timestamp_format() {
        let t = chrono_now();
        let bytes = t.as_bytes();
        assert_eq!(bytes.len(), 23, "timestamp {:?}", t);
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[7..8], "-");
        assert_eq!(&t[10..11], " ");
        assert_eq!(&t[13..14], ":");
        assert_eq!(&t[16..17], ":");
        assert_eq!(&t[19..20], ".");
        assert!(t[..4].bytes().all(|b| b.is_ascii_digit()));
    }

    /// Env detection is process-global; keep all env assertions in one
    /// test so they can't race with other tests in this binary.
    #[test]
    fn environment_detection() {
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        std::env::remove_var("XDG_SESSION_TYPE");
        std::env::remove_var("DISPLAY");
        assert_eq!(detect_env(), DesktopEnv::Wayland);

        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::set_var("XDG_SESSION_TYPE", "wayland");
        assert_eq!(detect_env(), DesktopEnv::Wayland);

        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::set_var("XDG_SESSION_TYPE", "x11");
        std::env::set_var("DISPLAY", ":0");
        assert_eq!(detect_env(), DesktopEnv::X11);

        // XWayland: both display vars set, Wayland must win
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        std::env::set_var("XDG_SESSION_TYPE", "x11");
        std::env::set_var("DISPLAY", ":0");
        assert_eq!(detect_env(), DesktopEnv::Wayland);

        // No display server at all: safe Wayland fallback
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("XDG_SESSION_TYPE");
        std::env::remove_var("DISPLAY");
        assert_eq!(detect_env(), DesktopEnv::Wayland);

        std::env::set_var("XDG_CURRENT_DESKTOP", "sway");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Sway);
        std::env::set_var("XDG_CURRENT_DESKTOP", "KDE");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::KDE);
        std::env::set_var("XDG_CURRENT_DESKTOP", "GNOME");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Gnome);
        std::env::set_var("XDG_CURRENT_DESKTOP", "Hyprland");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Hyprland);
        std::env::set_var("XDG_CURRENT_DESKTOP", "ubuntu:GNOME");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Gnome);
        std::env::set_var("XDG_CURRENT_DESKTOP", "unknown");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Other);
    }
}
