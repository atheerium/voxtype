use anyhow::{Context, Result};
use reqwest::multipart;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use tokio::process::Command as TokioCommand;
use tokio::signal::unix::{signal, SignalKind};

use crate::config::Config;
use crate::stats::Stats;

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
    Kde,
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
        WaylandCompositor::Kde
    } else if desktop.contains("gnome") || desktop.contains("mutter") {
        WaylandCompositor::Gnome
    } else {
        WaylandCompositor::Other
    }
}

/// The effective backend: the `backend` config override wins when set,
/// otherwise auto-detection. Used everywhere backend choice matters
/// (dependency checks, install hints, text injection) so a forced
/// `backend = "x11"` behaves the same in each path.
fn effective_env() -> DesktopEnv {
    match Config::load().ok().and_then(|c| c.backend) {
        Some(b) if b == "x11" => DesktopEnv::X11,
        Some(b) if b == "wayland" => DesktopEnv::Wayland,
        _ => detect_env(),
    }
}

pub fn detect_audio_system() -> AudioSystem {
    // PipeWire >= 0.3 provides a pulse-compatible socket at the same path
    let pulse_info = Command::new("pactl").args(["info"]).output();
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
            let display = std::env::var("DISPLAY").map_err(|_| {
                anyhow::anyhow!(
                    "DISPLAY is not set. voxtype needs an X11 display.\n\
                     Make sure you're running this from within an X session.\n\
                     If using Wayland, set backend = \"wayland\" in config.toml."
                )
            })?;
            if display.is_empty() {
                anyhow::bail!("DISPLAY is set but empty. Check your X11 session.");
            }
        }
        DesktopEnv::Wayland => {
            let wl = std::env::var("WAYLAND_DISPLAY").map_err(|_| {
                anyhow::anyhow!(
                    "WAYLAND_DISPLAY is not set. voxtype needs a Wayland compositor.\n\
                     Make sure you're running this from within a Wayland session.\n\
                     If using X11, set backend = \"x11\" in config.toml."
                )
            })?;
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
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                        "Cannot determine Wayland runtime directory (XDG_RUNTIME_DIR is not set)"
                    )
                    })?;
                runtime_dir.join(&wl)
            };
            if !socket.exists() {
                anyhow::bail!(
                    "Wayland socket {} does not exist. Check your compositor.",
                    socket.display()
                );
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
    fs::read_to_string(PIDFILE)
        .ok()
        .and_then(|c| c.trim().parse::<u32>().ok())
}

pub fn process_alive(pid: u32) -> bool {
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
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let (y, mo, da) = civil_from_days((secs / 86400) as i64);
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        y,
        mo,
        da,
        h,
        m,
        s,
        d.subsec_millis()
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
    fs::read_to_string(LOCKFILE)
        .ok()
        .and_then(|c| c.trim().parse::<u32>().ok())
}

fn kill_ffmpeg() {
    if let Some(pid) = read_lockfile_pid() {
        let _ = Command::new("kill").arg(pid.to_string()).output();
    }
}

/// Wait for the audio file to be written and ffmpeg to finish flushing.
/// Returns early once the ffmpeg process has exited (its output buffers
/// are flushed), rather than always sleeping the full `timeout`. The
/// timeout is a ceiling; the actual wait is typically one 50 ms poll.
async fn wait_for_audio_file(path: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let check_interval = Duration::from_millis(50);
    loop {
        // If the ffmpeg process is gone AND the file exists, its buffers
        // have been flushed and synced to disk.
        if !is_ffmpeg_running() && Path::new(path).exists() {
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(check_interval).await;
    }
}

/// True if a process named `ffmpeg` is currently running (i.e. the lockfile
/// PID still resolves to an ffmpeg process).
fn is_ffmpeg_running() -> bool {
    read_lockfile_pid()
        .map(|pid| is_process_named(pid, "ffmpeg"))
        .unwrap_or(false)
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
    let env = effective_env();
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
/// Bounded: a wedged notification daemon must not hold up the toggle loop.
/// Falls back to stderr (useful when running from terminal or
/// when no notification daemon is installed).
fn notify(summary: &str, body: &str) {
    let mut cmd = Command::new("notify-send");
    cmd.args(["-a", "voxtype", summary, body]);
    let sent = matches!(
        run_limited(&mut cmd, NOTIFY_TIMEOUT),
        CommandOutcome::Completed(o) if o.status.success()
    );

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

    // Register signal handlers BEFORE the PID file exists. The CLI only
    // sends SIGUSR1 after it observes the PID file, so writing it before
    // the handlers were installed would let a toggle arrive while SIGUSR1
    // still had its default disposition, which kills the process.
    let mut usr1 =
        signal(SignalKind::user_defined1()).context("Failed to setup SIGUSR1 handler")?;
    let mut term = signal(SignalKind::terminate()).context("Failed to setup SIGTERM handler")?;
    let mut int = signal(SignalKind::interrupt()).context("Failed to setup SIGINT handler")?;

    // A previous daemon may have died mid-recording (crash or SIGKILL),
    // leaving an orphaned ffmpeg and a stale lockfile behind. Clean these
    // up so a fresh recording can't conflict with leftover state.
    cleanup_stale_state();

    // Write PID file. Must come after handler registration: this file is
    // the CLI's readiness marker, so its existence guarantees a toggle can
    // be handled rather than kill the daemon.
    fs::write(PIDFILE, std::process::id().to_string()).context("Failed to write PID file")?;

    // Validate environment and dependencies. effective_env() honors a
    // forced `backend` config override, keeping the startup log, display
    // check, and dependency validation consistent with inject_text.
    let env = effective_env();
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
        write_log(
            "WARNING: No audio system detected (pactl/pipewire not found). Recording will fail.",
        );
        eprintln!(
            "voxtype WARNING: No audio system detected. Install pulseaudio-utils or pipewire."
        );
    } else {
        write_log(&format!("Audio system: {:?}", audio));
    }

    // Validate runtime dependencies
    let missing = validate_deps();
    if !missing.is_empty() {
        let msg = format!(
            "Missing runtime dependencies: {}. Install with: {}",
            missing.join(", "),
            deps_install_hint(effective_env())
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
        anyhow::bail!("ffmpeg not found. Install: sudo apt install ffmpeg");
    }

    // Check audio system
    let audio = detect_audio_system();
    match audio {
        AudioSystem::PulseAudio | AudioSystem::PipeWire => {
            // pactl info works for both pulseaudio and pipewire-pulse
            let pa_check = Command::new("pactl").args(["info"]).output();
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
            "-f",
            "pulse",
            "-i",
            &audio_source,
            "-ac",
            "1",
            "-ar",
            "16000",
            "-b:a",
            "64k",
            "-loglevel",
            "error",
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

    // Wait for ffmpeg to terminate and finalize the audio file.
    // Polling the file size stabilises once ffmpeg has flushed its buffers
    // and exited; most of the time this is far less than the old fixed 400 ms.
    wait_for_audio_file(AUDIO_FILE, Duration::from_millis(400)).await;

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

    if audio_too_large(meta.len() as usize) {
        let _ = fs::remove_file(AUDIO_FILE);
        anyhow::bail!(
            "Audio file too large ({} MB). Maximum is ~20 MB.\n\
             Speak for a shorter duration or reduce bitrate.",
            meta.len() / 1_000_000
        );
    }

    let text = transcribe_with_fallback(&config).await?;

    if text.trim().is_empty() {
        let _ = fs::remove_file(AUDIO_FILE);
        anyhow::bail!("Transcription returned empty (no speech detected)");
    }

    // Check that we can inject before removing the audio file. Injection
    // spawns external tools; run it on a blocking thread so a wedged tool
    // (every step is timeout-bounded inside inject_text) never freezes the
    // async runtime or drops queued toggles.
    let inject = text.clone();
    tokio::task::spawn_blocking(move || inject_text(&inject))
        .await
        .context("Injection task panicked")??;

    let _ = fs::remove_file(AUDIO_FILE);
    Ok(text)
}

/// Fallback chain driven by historical provider performance.
///
/// When `default_provider` is a specific name ("deepgram", "mistral",
/// "groq"), only that provider is tried. When it is "auto", providers are
/// ordered by `Stats::ranked_providers`: reliability (Laplace-smoothed
/// success rate) is the primary key, average latency the tiebreaker.
/// This means the most reliable provider is always attempted first,
/// minimising time lost to failed calls, while the fastest provider
/// wins when reliability is tied.
async fn transcribe_with_fallback(config: &Config) -> Result<String> {
    let provider = config.default_provider();
    let mut stats = Stats::load();

    // If a specific provider is configured (not "auto"), only try that one.
    if provider != "auto" {
        let result = match provider {
            "deepgram" => {
                try_provider(
                    "deepgram",
                    || async {
                        let key = config.deepgram_api_key()?;
                        transcribe_deepgram(key).await
                    },
                    &mut stats,
                )
                .await
            }
            "mistral" => {
                try_provider(
                    "mistral",
                    || async {
                        let key = config.mistral_api_key()?;
                        transcribe_mistral(&key, config).await
                    },
                    &mut stats,
                )
                .await
            }
            "groq" => {
                try_provider(
                    "groq",
                    || async {
                        let key = config.groq_api_key()?;
                        transcribe_groq(&key, config).await
                    },
                    &mut stats,
                )
                .await
            }
            _ => anyhow::bail!(
                "Unknown default_provider '{}'. Use deepgram, mistral, groq, or auto.",
                provider
            ),
        };
        let _ = stats.save();
        return result.map(|t| t.trim().to_string());
    }

    // Auto mode: rank available providers by historical reliability + latency.
    let available: Vec<String> = ["deepgram", "mistral", "groq"]
        .iter()
        .copied()
        .filter(|p| provider_key(config, p).is_ok())
        .map(String::from)
        .collect();

    if available.is_empty() {
        anyhow::bail!(
            "No STT API keys configured. Set at least one of:\n  \
             DEEPGRAM_API_KEY=...\n  GROQ_API_KEY=...\n  MISTRAL_API_KEY=..."
        );
    }

    let ranked = stats.ranked_providers(&available);
    write_log(&format!("Provider ranking: {}", ranked.join(" > ")));

    // Try each provider in ranked order; first non-empty success wins.
    for name in &ranked {
        let result = try_provider(name, || provider_call(config, name), &mut stats).await;
        match result {
            Ok(text) if !text.trim().is_empty() => {
                let _ = stats.save();
                return Ok(text.trim().to_string());
            }
            Ok(_) => {
                write_log(&format!("{} returned empty; trying next", name));
            }
            Err(e) => {
                write_log(&format!("{} failed: {}; trying next provider", name, e));
            }
        }
        let _ = stats.save();
    }

    anyhow::bail!("All configured providers failed or returned empty results")
}

/// Resolve the API key for a provider name, returning Ok only when a key
/// is available (file config or env var).
fn provider_key(config: &Config, name: &str) -> Result<()> {
    match name {
        "deepgram" => config.deepgram_api_key().map(|_| ()),
        "mistral" => config.mistral_api_key().map(|_| ()),
        "groq" => config.groq_api_key().map(|_| ()),
        _ => anyhow::bail!("Unknown provider '{}'", name),
    }
}

/// Build the transcription call for a provider name. The key is resolved
/// inside so that missing-key errors propagate cleanly to the caller.
async fn provider_call(config: &Config, name: &str) -> Result<String> {
    match name {
        "deepgram" => {
            let key = config.deepgram_api_key()?;
            transcribe_deepgram(key).await
        }
        "mistral" => {
            let key = config.mistral_api_key()?;
            transcribe_mistral(&key, config).await
        }
        "groq" => {
            let key = config.groq_api_key()?;
            transcribe_groq(&key, config).await
        }
        _ => anyhow::bail!("Unknown provider '{}'", name),
    }
}

/// Time a single provider attempt and record its result in stats.
async fn try_provider<F, Fut>(name: &str, call: F, stats: &mut Stats) -> Result<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let start = Instant::now();
    let result = call().await;
    let elapsed = start.elapsed().as_millis() as u64;
    match &result {
        Ok(text) => stats.record_success(name, elapsed, text.len()),
        Err(_) => stats.record_failure(name, elapsed),
    }
    result
}

async fn transcribe_deepgram(api_key: String) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create HTTP client")?;

    let audio_bytes = fs::read(AUDIO_FILE).context("Failed to read audio file for upload")?;

    let response = client
        .post("https://api.deepgram.com/v1/listen")
        .header("Authorization", format!("Token {}", api_key))
        .query(&[
            ("model", "nova-3"),
            ("punctuate", "true"),
            ("smart_format", "true"),
        ])
        .header("Content-Type", "audio/mpeg")
        .body(audio_bytes)
        .send()
        .await
        .context("Failed to reach Deepgram API (check network/internet)")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read Deepgram API response")?;

    if !status.is_success() {
        let hint = match status.as_u16() {
            401 => "\nHint: Your DEEPGRAM_API_KEY is invalid. Check ~/.config/voxtype/config.toml or your shell rc file.",
            403 | 429 => "\nHint: Deepgram rate limit or quota exceeded. Falling back to next provider.",
            413 => "\nHint: Audio file too large for Deepgram's API limit.",
            _ => "",
        };
        anyhow::bail!("Deepgram API error (HTTP {}): {}{}", status, body, hint);
    }

    let json: serde_json::Value =
        serde_json::from_str(&body).context("Failed to parse Deepgram JSON response")?;

    let transcript = json["results"]["channels"][0]["alternatives"][0]["transcript"]
        .as_str()
        .context("Deepgram response missing transcript field")?;

    Ok(transcript.to_string())
}

async fn transcribe_mistral(api_key: &str, config: &Config) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create HTTP client")?;

    let audio_bytes = fs::read(AUDIO_FILE).context("Failed to read audio file for upload")?;

    let file_part = multipart::Part::bytes(audio_bytes)
        .file_name("recording.mp3")
        .mime_str("audio/mpeg")
        .context("Invalid MIME type")?;

    let mut form = multipart::Form::new()
        .part("file", file_part)
        .text("model", "voxtral-mini-latest");

    if let Some(lang) = config.language() {
        form = form.text("language", lang.to_string());
    }

    let response = client
        .post("https://api.mistral.ai/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .context("Failed to reach Mistral API (check network/internet)")?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read Mistral API response")?;

    if !status.is_success() {
        let hint = match status.as_u16() {
            401 => "\nHint: Your MISTRAL_API_KEY is invalid. Check ~/.config/voxtype/config.toml or your shell rc file.",
            402 | 429 => "\nHint: Mistral rate limit exceeded. Falling back to next provider.",
            413 => "\nHint: Audio file too large for Mistral's API limit.",
            _ => "",
        };
        anyhow::bail!("Mistral API error (HTTP {}): {}{}", status, body, hint);
    }

    let json: serde_json::Value =
        serde_json::from_str(&body).context("Failed to parse Mistral JSON response")?;

    let text = json["text"]
        .as_str()
        .context("Mistral response missing 'text' field")?;

    Ok(text.to_string())
}

async fn transcribe_groq(api_key: &str, config: &Config) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create HTTP client")?;

    let audio_bytes = fs::read(AUDIO_FILE).context("Failed to read audio file for upload")?;

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
    let body = response
        .text()
        .await
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

    anyhow::bail!("Empty response from Groq API (unexpected)")
}

// ── Text Injection ────────────────────────────────────────────

/// Upper bound for clipboard-set commands (wl-copy, xsel, xclip). These
/// daemonize after writing their data; a parent that stays alive means the
/// selection was never offered, so we kill it and report instead of blocking.
const CLIP_CMD_TIMEOUT: Duration = Duration::from_secs(3);
/// Per-attempt budget for a wtype paste. The virtual-keyboard protocol can
/// wedge under compositor load; never let it block dictation indefinitely.
const WTYPE_TIMEOUT: Duration = Duration::from_millis(2500);
/// Budget for verifying the clipboard via wl-paste before sending the key.
/// wl-paste blocks when the clipboard is empty, so this is the per-attempt
/// ceiling. The retry loop means total verify time is at most 2 × this.
const VERIFY_TIMEOUT: Duration = Duration::from_millis(500);
/// Budget for compositor IPC (swaymsg / hyprctl) used in focus detection.
const IPC_TIMEOUT: Duration = Duration::from_millis(800);
/// Budget for desktop notifications; a wedged notification daemon must not
/// hold up the toggle.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);
/// Fixed sleep to let the X11 clipboard propagate after xsel/xclip sets it.
/// wl-copy and its X11 analogues report success once the data is registered;
/// a short sleep avoids skipping the paste before the selection is live.
const CLIPBOARD_PROPAGATE_DELAY: Duration = Duration::from_millis(50);

/// Outcome of a bounded external command run.
#[derive(Debug)]
enum CommandOutcome {
    Completed(std::process::Output),
    TimedOut,
    Failed(String),
}

/// Run `cmd` to completion, killing it if it exceeds `limit`.
///
/// When `capture` is false, stdout/stderr go to /dev/null. This matters for
/// daemonizing tools (wl-copy, xsel, xclip): they fork a child that inherits
/// the std fds, and a pipe held open by that child would block the reader
/// forever. When `capture` is true the streams are drained on background
/// threads so a chatty child can never deadlock the wait. `input`, when set,
/// is written to the child's stdin on a background thread.
fn run_limited_with_stdin(
    cmd: &mut Command,
    limit: Duration,
    input: Option<&[u8]>,
    capture: bool,
) -> CommandOutcome {
    if capture {
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }
    if input.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return CommandOutcome::Failed(e.to_string()),
    };

    if let (Some(mut stdin), Some(data)) = (child.stdin.take(), input) {
        let data = data.to_vec();
        let _ = thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }

    let stdout_thread = if capture {
        child.stdout.take().map(|mut s| {
            thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                buf
            })
        })
    } else {
        None
    };
    let stderr_thread = if capture {
        child.stderr.take().map(|mut s| {
            thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf);
                buf
            })
        })
    } else {
        None
    };

    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return CommandOutcome::TimedOut;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return CommandOutcome::Failed(e.to_string()),
        }
    };

    let stdout = stdout_thread
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_thread
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    CommandOutcome::Completed(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Bounded run without output capture (for daemonizing tools).
fn run_limited(cmd: &mut Command, limit: Duration) -> CommandOutcome {
    run_limited_with_stdin(cmd, limit, None, false)
}

/// Bounded run with stdout/stderr captured (for one-shot tools).
fn run_limited_capture(cmd: &mut Command, limit: Duration) -> CommandOutcome {
    run_limited_with_stdin(cmd, limit, None, true)
}

fn inject_text(text: &str) -> Result<()> {
    let env = effective_env();

    // Validate display env before attempting injection
    check_display_env(env)?;

    match env {
        DesktopEnv::X11 => inject_text_x11(text),
        DesktopEnv::Wayland => inject_text_wayland(text),
    }
}

/// True when an X11 toolchain (clipboard + keystroke) is available, i.e. the
/// session exposes DISPLAY and XWayland tooling is installed.
fn x11_tools_available() -> bool {
    std::env::var("DISPLAY")
        .map(|d| !d.is_empty())
        .unwrap_or(false)
        && (require_tool("xsel") || require_tool("xclip"))
        && require_tool("xdotool")
}

fn inject_text_x11(text: &str) -> Result<()> {
    // 1. Set clipboard via xsel (primary)
    let xsel_ok = set_clipboard_xsel(text).is_ok();

    // 2. Fallback to xclip if xsel fails
    if !xsel_ok {
        set_clipboard_xclip(text).context(
            "Both xsel and xclip failed to set clipboard. Install: sudo apt install xsel xclip",
        )?;
    }

    // 3. Wait for clipboard propagation
    std::thread::sleep(CLIPBOARD_PROPAGATE_DELAY);

    // 4. VS Code has keyboard shortcut conflicts with xdotool paste
    if is_vscode_window_x11() {
        write_log(
            "VS Code detected — skipping keyboard paste (clipboard set). Manual paste: Ctrl+V",
        );
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
    let mut cmd = Command::new("xdotool");
    cmd.args(["key", shortcut]);
    match run_limited(&mut cmd, CLIP_CMD_TIMEOUT) {
        CommandOutcome::Completed(o) if o.status.success() => Ok(()),
        CommandOutcome::Completed(o) => anyhow::bail!(
            "xdotool key {} failed ({}). Clipboard is set (manual paste). Is DISPLAY set correctly?",
            shortcut,
            o.status
        ),
        CommandOutcome::TimedOut => anyhow::bail!(
            "xdotool key {} timed out. Clipboard is set (manual paste).",
            shortcut
        ),
        CommandOutcome::Failed(e) => anyhow::bail!(
            "xdotool key {} failed ({}). Clipboard is set (manual paste).",
            shortcut,
            e
        ),
    }
}

fn inject_text_wayland(text: &str) -> Result<()> {
    let compositor = detect_wayland_compositor();

    // 0. Identify the focused window (best effort) so the paste shortcut and
    //    injection backend match the app. Authoritative on Sway/Hyprland via
    //    compositor IPC; elsewhere we fall back to the compositor-aware key
    //    sequence instead of guessing from possibly-stale X11 focus.
    let target = detect_focus_target(compositor);
    match &target {
        Some(t) => write_log(&format!(
            "Focus: {} (id={:?}, {})",
            match t.class {
                FocusClass::Terminal => "terminal",
                FocusClass::Browser => "browser",
                FocusClass::Ide => "IDE",
                FocusClass::Generic => "generic",
            },
            t.id,
            if t.xwayland {
                "XWayland"
            } else {
                "native Wayland"
            }
        )),
        None => write_log("Focus detection unavailable — using generic paste sequence"),
    }

    // XWayland windows are injected through the X11 path: XTEST events reach
    // the client directly and the X11 clipboard is used, skipping the
    // Wayland↔X clipboard bridge that is a common source of paste lag.
    if target.as_ref().is_some_and(|t| t.xwayland) {
        if x11_tools_available() {
            write_log("Focused window is XWayland — injecting via X11 path");
            return inject_text_x11(text);
        }
        write_log("Focused window is XWayland but X11 tools are missing — using Wayland path");
    }

    // 1. Set the Wayland clipboard and verify the data offer is live before
    //    sending the paste key. wl-copy daemonizes asynchronously; pasting
    //    before the offer is registered is the classic "paste did nothing"
    //    race in browsers.
    if !require_tool("wl-copy") {
        if x11_tools_available() {
            write_log("wl-copy missing — using X11 injection fallback");
            return inject_text_x11(text);
        }
        anyhow::bail!("wl-copy not found. Install: sudo apt install wl-clipboard");
    }
    let t0 = Instant::now();
    set_clipboard_wl_copy_verified(text)?;
    write_log(&format!(
        "Clipboard set and verified in {} ms",
        t0.elapsed().as_millis()
    ));

    // Insurance: an XWayland client we could not detect (KDE/GNOME without
    // compositor IPC) reads the X11 CLIPBOARD. Seed it too when the target is
    // unknown, so the compositor's slow bridge never has to proxy the data.
    maybe_set_x_clipboard(text, target.as_ref());

    // 2. Skip keyboard paste for IDEs (shortcut conflicts); the clipboard is
    //    already set for a manual paste.
    if target.as_ref().is_some_and(|t| t.class == FocusClass::Ide) || is_vscode_running() {
        write_log("IDE focused — skipping keyboard paste (clipboard set). Manual paste: Ctrl+V / Ctrl+Shift+V");
        return Ok(());
    }

    // 3. wtype must exist to send the paste key.
    if !require_tool("wtype") {
        write_log(&format!(
            "wtype not found. Copied {} chars to clipboard (manual paste: Ctrl+Shift+V / Ctrl+V). Install: sudo apt install wtype",
            text.len()
        ));
        return Ok(());
    }

    // 4. Send the paste key(s). Known targets get exactly one shortcut
    //    (terminals: Ctrl+Shift+V, browsers: Ctrl+V) — the wrong first key in
    //    a chain is what causes double pastes or stray `^V` in terminals.
    if !paste_with_wtype(target.as_ref(), compositor) {
        write_log("wtype paste failed — clipboard is set (manual paste: Ctrl+Shift+V / Ctrl+V)");
    }

    Ok(())
}

/// Paste via wtype, choosing the key sequence from the focused app.
/// Returns true if at least one attempt was delivered (exit 0).
fn paste_with_wtype(target: Option<&FocusTarget>, compositor: WaylandCompositor) -> bool {
    let keys_list = paste_keys_for_target(target, compositor);
    let mut last_error = String::new();

    for keys in keys_list {
        write_log(&format!("wtype paste attempt: {:?}", keys));
        let mut cmd = Command::new("wtype");
        cmd.args(*keys);
        match run_limited_capture(&mut cmd, WTYPE_TIMEOUT) {
            CommandOutcome::Completed(o) if o.status.success() => {
                write_log("wtype paste delivered");
                return true;
            }
            CommandOutcome::Completed(o) => {
                last_error = String::from_utf8_lossy(&o.stderr).trim().to_string();
                write_log(&format!("wtype attempt failed: {}", last_error));

                // Retry the same key once: a non-zero exit means the key was
                // not delivered (e.g. the compositor was busy), so repeating
                // it cannot double-paste.
                thread::sleep(Duration::from_millis(80));
                let mut retry = Command::new("wtype");
                retry.args(*keys);
                match run_limited_capture(&mut retry, WTYPE_TIMEOUT) {
                    CommandOutcome::Completed(o2) if o2.status.success() => {
                        write_log("wtype paste delivered on retry");
                        return true;
                    }
                    CommandOutcome::Completed(o2) => {
                        last_error = String::from_utf8_lossy(&o2.stderr).trim().to_string();
                    }
                    CommandOutcome::TimedOut => {
                        last_error = format!("timed out after {} ms", WTYPE_TIMEOUT.as_millis());
                    }
                    CommandOutcome::Failed(e) => last_error = e,
                }
            }
            CommandOutcome::TimedOut => {
                // No retry on timeout: a wedged virtual keyboard will likely
                // wedge again, and we must bound total injection time.
                last_error = format!("timed out after {} ms", WTYPE_TIMEOUT.as_millis());
                write_log(&format!("wtype timed out: {}", last_error));
            }
            CommandOutcome::Failed(e) => {
                last_error = e;
                write_log(&format!("wtype error: {}", last_error));
            }
        }
    }

    write_log(&format!("All wtype paste attempts failed ({})", last_error));
    false
}

fn set_clipboard_wl_copy(text: &str) -> Result<()> {
    let mut cmd = Command::new("wl-copy");
    match run_limited_with_stdin(&mut cmd, CLIP_CMD_TIMEOUT, Some(text.as_bytes()), false) {
        CommandOutcome::Completed(o) if o.status.success() => Ok(()),
        CommandOutcome::Completed(o) => Err(anyhow::anyhow!("wl-copy exited with {}", o.status)),
        CommandOutcome::TimedOut => Err(anyhow::anyhow!(
            "wl-copy timed out after {} ms",
            CLIP_CMD_TIMEOUT.as_millis()
        )),
        CommandOutcome::Failed(e) => Err(anyhow::anyhow!(
            "wl-copy not found: {}. Install: sudo apt install wl-clipboard",
            e
        )),
    }
}

/// Set the Wayland clipboard and confirm the data offer is live by reading
/// it back with wl-paste, retrying once. Pasting before the offer is
/// registered is why a paste key sometimes lands in a browser with nothing
/// to paste.
fn set_clipboard_wl_copy_verified(text: &str) -> Result<()> {
    let mut verified = false;
    for attempt in 1..=2 {
        set_clipboard_wl_copy(text)?;
        if clipboard_contains(text) {
            verified = true;
            break;
        }
        write_log(&format!(
            "wl-copy attempt {} not visible to wl-paste — retrying",
            attempt
        ));
    }
    if !verified {
        // Last chance: set it once more and proceed. The offer may still
        // become visible to the target app even if our read-back raced.
        set_clipboard_wl_copy(text)?;
        write_log("WARNING: clipboard not verified after retries — pasting anyway");
    }
    Ok(())
}

/// Read the Wayland clipboard back and compare it to what we set.
fn clipboard_contains(expected: &str) -> bool {
    let mut cmd = Command::new("wl-paste");
    match run_limited_capture(&mut cmd, VERIFY_TIMEOUT) {
        CommandOutcome::Completed(o) if o.status.success() => {
            clipboard_matches(expected, String::from_utf8_lossy(&o.stdout).trim())
        }
        _ => false,
    }
}

/// Compare the text we set on the clipboard with what we read back,
/// tolerating a trailing newline added or stripped by the clipboard.
fn clipboard_matches(expected: &str, got: &str) -> bool {
    expected.trim_end_matches('\n') == got.trim_end_matches('\n')
}

/// Seed the X11 CLIPBOARD selection in addition to Wayland's. XWayland apps
/// (e.g. Chrome under XWayland) read the X selection; keeping both in sync
/// avoids relying on the compositor's Wayland↔X bridge. Only done when the
/// target is unknown, when the window could plausibly be XWayland.
fn maybe_set_x_clipboard(text: &str, target: Option<&FocusTarget>) {
    let worth_it = match target.map(|t| t.class) {
        None | Some(FocusClass::Generic) => true,
        Some(_) => false,
    };
    if !worth_it {
        return;
    }
    if std::env::var("DISPLAY")
        .map(|d| d.is_empty())
        .unwrap_or(true)
    {
        return;
    }
    let mut used = false;
    if require_tool("xsel") {
        used = set_clipboard_xsel(text).is_ok();
    }
    if !used && require_tool("xclip") {
        used = set_clipboard_xclip(text).is_ok();
    }
    if used {
        write_log("Seeded X11 clipboard for XWayland compatibility");
    }
}

fn set_clipboard_xsel(text: &str) -> Result<()> {
    let mut cmd = Command::new("xsel");
    cmd.args(["--clipboard", "--input"]);
    match run_limited_with_stdin(&mut cmd, CLIP_CMD_TIMEOUT, Some(text.as_bytes()), false) {
        CommandOutcome::Completed(o) if o.status.success() => Ok(()),
        CommandOutcome::Completed(o) => Err(anyhow::anyhow!(
            "xsel exited with {}. Install: sudo apt install xsel",
            o.status
        )),
        CommandOutcome::TimedOut => Err(anyhow::anyhow!(
            "xsel timed out after {} ms",
            CLIP_CMD_TIMEOUT.as_millis()
        )),
        CommandOutcome::Failed(e) => Err(anyhow::anyhow!(
            "xsel not found: {}. Install: sudo apt install xsel",
            e
        )),
    }
}

fn set_clipboard_xclip(text: &str) -> Result<()> {
    let mut cmd = Command::new("xclip");
    cmd.args(["-selection", "clipboard"]);
    match run_limited_with_stdin(&mut cmd, CLIP_CMD_TIMEOUT, Some(text.as_bytes()), false) {
        CommandOutcome::Completed(o) if o.status.success() => Ok(()),
        CommandOutcome::Completed(o) => Err(anyhow::anyhow!(
            "xclip exited with {}. Install: sudo apt install xclip",
            o.status
        )),
        CommandOutcome::TimedOut => Err(anyhow::anyhow!(
            "xclip timed out after {} ms",
            CLIP_CMD_TIMEOUT.as_millis()
        )),
        CommandOutcome::Failed(e) => Err(anyhow::anyhow!(
            "xclip not found: {}. Install: sudo apt install xclip",
            e
        )),
    }
}
// ── Window classification helpers ──────────────────────────────

/// Extract the class from an `xprop -id <win> WM_CLASS` output line,
/// e.g. `WM_CLASS(STRING) = "alacritty", "Alacritty"` -> `alacritty`.
fn parse_wm_class(output: &str) -> Option<String> {
    let class = output.rsplit('"').nth(1)?.trim().to_lowercase();
    if class.is_empty() {
        None
    } else {
        Some(class)
    }
}

fn is_known_terminal(name: &str) -> bool {
    const KNOWN_TERMINALS: &[&str] = &[
        "alacritty",
        "xfce4-terminal",
        "xfterminal",
        "gnome-terminal",
        "gnome-terminal-server",
        "konsole",
        "xterm",
        "uxterm",
        "urxvt",
        "urxvtc",
        "terminator",
        "tilix",
        "kitty",
        "wezterm",
        "st",
        "st-256color",
        "rxvt",
        "foot",
        "footclient",
        "guake",
        "mate-terminal",
        "lxterminal",
        "cool-retro-term",
        "deepin-terminal",
        "sakura",
        "termite",
        "ghostty",
        "blackbox",
        "contour",
        "tabby",
        "warp-terminal",
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
            &[
                "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "ctrl", "-m", "shift",
            ],
        ],
        // Sway/Hyprland/KDE: terminals common, try Ctrl+Shift+V first
        _ => &[
            &[
                "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "ctrl", "-m", "shift",
            ],
            &["-M", "ctrl", "-k", "v", "-m", "ctrl"],
        ],
    }
}

/// Paste key sequences to use, ordered by likelihood, for the focused app.
/// Known classes get exactly one sequence: the wrong first key in a fallback
/// chain is what causes double pastes (browsers accept Ctrl+Shift+V too) and
/// stray literal characters (Ctrl+V is "quoted insert" in many terminals).
fn paste_keys_for_target(
    target: Option<&FocusTarget>,
    compositor: WaylandCompositor,
) -> &'static [&'static [&'static str]] {
    const TERM_KEYS: &[&[&str]] = &[&[
        "-M", "ctrl", "-M", "shift", "-k", "v", "-m", "ctrl", "-m", "shift",
    ]];
    const GUI_KEYS: &[&[&str]] = &[&["-M", "ctrl", "-k", "v", "-m", "ctrl"]];
    const NONE: &[&[&str]] = &[];

    match target.map(|t| t.class) {
        Some(FocusClass::Terminal) => TERM_KEYS,
        Some(FocusClass::Browser) => GUI_KEYS,
        Some(FocusClass::Ide) => NONE,
        Some(FocusClass::Generic) | None => paste_keys_for(compositor),
    }
}

// ── Focused-window detection (Wayland paste targeting) ────────

/// Broad app classes that dictate which paste shortcut to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusClass {
    Terminal,
    Browser,
    Ide,
    Generic,
}

/// A detected focused window.
#[derive(Debug, Clone, PartialEq)]
struct FocusTarget {
    class: FocusClass,
    /// True when the window is an XWayland (X11) client.
    xwayland: bool,
    /// Raw app identifier (app_id or WM_CLASS), when known.
    id: Option<String>,
}

const BROWSER_CLASSES: &[&str] = &[
    "google-chrome",
    "chrome",
    "chromium",
    "chromium-browser",
    "firefox",
    "firefox-esr",
    "librewolf",
    "waterfox",
    "brave-browser",
    "brave",
    "microsoft-edge",
    "msedge",
    "edge",
    "vivaldi",
    "opera",
    "epiphany",
    "zen",
    "thorium-browser",
    "floorp",
    "mullvad-browser",
    "tor-browser",
];

const IDE_CLASSES: &[&str] = &[
    "code",
    "code-oss",
    "vscodium",
    "codium",
    "cursor",
    "zed",
    "sublime_text",
    "sublimetext",
    "idea",
    "pycharm",
    "webstorm",
    "goland",
    "rustrover",
    "clion",
    "phpstorm",
    "datagrip",
    "rider",
    "rubymine",
    "fleet",
    "android-studio",
    "studio64",
];

/// Map an app identifier (sway `app_id`, Hyprland `class`, or X11 `WM_CLASS`)
/// to the class that determines the paste shortcut.
fn classify_focus(id: &str) -> FocusClass {
    let id = id.trim().to_lowercase();
    if id.is_empty() {
        return FocusClass::Generic;
    }
    if is_known_terminal(&id) {
        return FocusClass::Terminal;
    }
    if BROWSER_CLASSES.contains(&id.as_str()) {
        return FocusClass::Browser;
    }
    if IDE_CLASSES.contains(&id.as_str()) {
        return FocusClass::Ide;
    }
    FocusClass::Generic
}

/// Best-effort detection of the focused window for the paste step.
/// Authoritative on Sway and Hyprland (compositor IPC); None elsewhere so
/// injection degrades to the generic compositor-aware key sequence instead
/// of guessing from possibly-stale X11 focus.
fn detect_focus_target(compositor: WaylandCompositor) -> Option<FocusTarget> {
    match compositor {
        WaylandCompositor::Sway => focused_window_from_sway(),
        WaylandCompositor::Hyprland => focused_window_from_hypr(),
        WaylandCompositor::Kde | WaylandCompositor::Gnome | WaylandCompositor::Other => None,
    }
}

/// Query the focused node from sway's tree via `swaymsg -t get_tree`.
fn focused_window_from_sway() -> Option<FocusTarget> {
    let mut cmd = Command::new("swaymsg");
    cmd.args(["-t", "get_tree"]);
    let stdout = match run_limited_capture(&mut cmd, IPC_TIMEOUT) {
        CommandOutcome::Completed(o) if o.status.success() => o.stdout,
        _ => return None,
    };
    let tree: serde_json::Value = serde_json::from_slice(&stdout).ok()?;
    let node = find_focused_node(&tree)?;
    focus_target_from_node(node)
}

/// Walk a sway `get_tree` JSON value to the node with `focused: true`.
fn find_focused_node(v: &serde_json::Value) -> Option<&serde_json::Value> {
    if v.get("focused").and_then(|b| b.as_bool()) == Some(true) {
        return Some(v);
    }
    if let Some(children) = v.get("nodes").and_then(|n| n.as_array()) {
        for child in children {
            if let Some(f) = find_focused_node(child) {
                return Some(f);
            }
        }
    }
    if let Some(children) = v.get("floating_nodes").and_then(|n| n.as_array()) {
        for child in children {
            if let Some(f) = find_focused_node(child) {
                return Some(f);
            }
        }
    }
    None
}

/// Extract the app id and XWayland-ness from a sway tree node.
fn focus_target_from_node(node: &serde_json::Value) -> Option<FocusTarget> {
    // Native Wayland clients expose `app_id`; XWayland clients expose
    // `window_properties.class` and no `app_id`.
    let app_id = node
        .get("app_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let window_properties = node.get("window_properties");
    let win_class = window_properties
        .and_then(|p| p.get("class"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let id = app_id.or(win_class).filter(|id| !id.is_empty());
    Some(FocusTarget {
        class: classify_focus(id.as_deref().unwrap_or("")),
        xwayland: window_properties.is_some(),
        id,
    })
}

/// Query Hyprland's focused window via `hyprctl activewindow -j`.
fn focused_window_from_hypr() -> Option<FocusTarget> {
    let mut cmd = Command::new("hyprctl");
    cmd.args(["activewindow", "-j"]);
    let stdout = match run_limited_capture(&mut cmd, IPC_TIMEOUT) {
        CommandOutcome::Completed(o) if o.status.success() => o.stdout,
        _ => return None,
    };
    let v: serde_json::Value = serde_json::from_slice(&stdout).ok()?;
    let class = v.get("class").and_then(|c| c.as_str()).map(str::to_string);
    let xwayland = v.get("xwayland").and_then(|x| x.as_bool()).unwrap_or(false);
    let id = class.filter(|c| !c.is_empty());
    Some(FocusTarget {
        class: classify_focus(id.as_deref().unwrap_or("")),
        xwayland,
        id,
    })
}

/// Groq has a ~25 MB upload limit; reject anything approaching it early.
fn audio_too_large(len: usize) -> bool {
    len > 20_000_000
}

fn is_terminal_window() -> bool {
    // Step 1: Get active window ID via xdotool
    let winid = Command::new("xdotool").args(["getactivewindow"]).output();

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
                    if is_term {
                        return true;
                    }
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
                let proc_name = String::from_utf8_lossy(&ps_out.stdout)
                    .trim()
                    .to_lowercase();
                let is_term = is_known_terminal(&proc_name);
                write_log(&format!(
                    "PID {} process: '{}' -> {}",
                    pid_num,
                    proc_name,
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

    match Command::new("xprop")
        .args(["-id", &winid, "WM_CLASS"])
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if let Some(class) = parse_wm_class(&stdout) {
                let known_ides: &[&str] = &["code", "code-oss", "vscode", "vscodium"];
                let is_ide = known_ides.contains(&class.as_str());
                if is_ide {
                    write_log(&format!(
                        "Window WM_CLASS: '{}' -> IDE, skipping keyboard paste",
                        class
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
        assert_eq!(
            parse_wm_class(r#"WM_CLASS(STRING) = "code", "Code""#),
            Some("code".to_string())
        );
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
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Kde);
        std::env::set_var("XDG_CURRENT_DESKTOP", "GNOME");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Gnome);
        std::env::set_var("XDG_CURRENT_DESKTOP", "Hyprland");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Hyprland);
        std::env::set_var("XDG_CURRENT_DESKTOP", "ubuntu:GNOME");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Gnome);
        std::env::set_var("XDG_CURRENT_DESKTOP", "unknown");
        assert_eq!(detect_wayland_compositor(), WaylandCompositor::Other);
    }

    #[test]
    fn command_timeout_kills_hung_child() {
        let outcome = run_limited(Command::new("sleep").arg("5"), Duration::from_millis(150));
        assert!(matches!(outcome, CommandOutcome::TimedOut));
    }

    #[test]
    fn command_capture_reads_stdout() {
        let outcome = run_limited_capture(
            Command::new("sh").args(["-c", "printf 'hello world'"]),
            Duration::from_secs(2),
        );
        match outcome {
            CommandOutcome::Completed(o) => {
                assert!(o.status.success());
                assert_eq!(o.stdout, b"hello world".to_vec());
            }
            other => panic!("expected Completed, got {:?}", other),
        }
    }

    /// The wtype path uses capture + timeout together; a hung child must be
    /// killed promptly without the drain threads hanging the caller.
    #[test]
    fn capture_times_out_without_hanging() {
        let t0 = std::time::Instant::now();
        let outcome = run_limited_capture(
            Command::new("sh").args(["-c", "exec sleep 5"]),
            Duration::from_millis(200),
        );
        let elapsed = t0.elapsed();
        assert!(matches!(outcome, CommandOutcome::TimedOut));
        assert!(
            elapsed < Duration::from_secs(3),
            "capture+timeout took {:?}",
            elapsed
        );
    }

    /// A wedged notification daemon must not hold up the toggle: notify-send
    /// is bounded even when the real tool would hang forever.
    #[test]
    fn notify_bounded_when_daemon_hangs() {
        use std::os::unix::fs::PermissionsExt;

        let fake_dir = format!("/tmp/voxtype-fake-bin-{}", std::process::id());
        let _ = std::fs::create_dir_all(&fake_dir);
        let fake = format!("{}/notify-send", fake_dir);
        std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&fake, PermissionsExt::from_mode(0o755)).unwrap();

        let orig_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", fake_dir, orig_path));
        let t0 = std::time::Instant::now();
        notify("voxtype test", "bounded");
        let elapsed = t0.elapsed();
        let _ = std::fs::remove_dir_all(&fake_dir);
        std::env::set_var("PATH", orig_path);

        assert!(
            elapsed < Duration::from_secs(5),
            "notify with a hanging daemon took {:?}",
            elapsed
        );
    }

    #[test]
    fn command_stdin_payload_is_written() {
        let outcome = run_limited_with_stdin(
            Command::new("sh").args(["-c", "cat"]),
            Duration::from_secs(2),
            Some(b"payload".as_slice()),
            true,
        );
        match outcome {
            CommandOutcome::Completed(o) => {
                assert!(o.status.success());
                assert_eq!(o.stdout, b"payload".to_vec());
            }
            other => panic!("expected Completed, got {:?}", other),
        }
    }

    #[test]
    fn focus_classification() {
        assert_eq!(classify_focus("google-chrome"), FocusClass::Browser);
        assert_eq!(classify_focus("chromium"), FocusClass::Browser);
        assert_eq!(classify_focus("firefox"), FocusClass::Browser);
        assert_eq!(classify_focus("Alacritty"), FocusClass::Terminal);
        assert_eq!(classify_focus("alacritty"), FocusClass::Terminal);
        assert_eq!(classify_focus("foot"), FocusClass::Terminal);
        assert_eq!(classify_focus("code"), FocusClass::Ide);
        assert_eq!(classify_focus("pycharm"), FocusClass::Ide);
        assert_eq!(classify_focus("gimp"), FocusClass::Generic);
        assert_eq!(classify_focus(""), FocusClass::Generic);
    }

    #[test]
    fn sway_focus_parsing() {
        // Native Wayland window: app_id present, no window_properties.
        let tree: serde_json::Value = serde_json::from_str(
            r#"{
                "nodes": [{
                    "nodes": [{
                        "focused": true,
                        "app_id": "google-chrome",
                        "name": "ChatGPT - Google Chrome"
                    }]
                }]
            }"#,
        )
        .unwrap();
        let node = find_focused_node(&tree).unwrap();
        let target = focus_target_from_node(node).unwrap();
        assert_eq!(target.class, FocusClass::Browser);
        assert!(!target.xwayland);
        assert_eq!(target.id.as_deref(), Some("google-chrome"));

        // XWayland window: window_properties instead of app_id.
        let tree: serde_json::Value = serde_json::from_str(
            r#"{
                "nodes": [{
                    "focused": true,
                    "window_properties": { "class": "alacritty", "instance": "alacritty" }
                }]
            }"#,
        )
        .unwrap();
        let target = focus_target_from_node(find_focused_node(&tree).unwrap()).unwrap();
        assert_eq!(target.class, FocusClass::Terminal);
        assert!(target.xwayland);
        assert_eq!(target.id.as_deref(), Some("alacritty"));

        // No focused node at all.
        let tree: serde_json::Value = serde_json::from_str(r#"{"nodes": []}"#).unwrap();
        assert!(find_focused_node(&tree).is_none());
    }

    #[test]
    fn paste_keys_for_known_targets() {
        let term = FocusTarget {
            class: FocusClass::Terminal,
            xwayland: false,
            id: Some("alacritty".to_string()),
        };
        let keys = paste_keys_for_target(Some(&term), WaylandCompositor::Sway);
        assert_eq!(keys.len(), 1);
        assert!(keys[0].contains(&"shift"));

        let browser = FocusTarget {
            class: FocusClass::Browser,
            xwayland: false,
            id: Some("google-chrome".to_string()),
        };
        let keys = paste_keys_for_target(Some(&browser), WaylandCompositor::Sway);
        assert_eq!(keys.len(), 1);
        assert!(!keys[0].contains(&"shift"));

        let ide = FocusTarget {
            class: FocusClass::Ide,
            xwayland: false,
            id: Some("code".to_string()),
        };
        assert!(paste_keys_for_target(Some(&ide), WaylandCompositor::Sway).is_empty());

        // Unknown target keeps the compositor-aware fallback chain.
        assert_eq!(
            paste_keys_for_target(None, WaylandCompositor::Sway).len(),
            2
        );
        assert_eq!(
            paste_keys_for_target(None, WaylandCompositor::Gnome).len(),
            2
        );
    }

    #[test]
    fn clipboard_match_comparison() {
        assert!(clipboard_matches("hello", "hello"));
        assert!(clipboard_matches("hello", "hello\n"));
        assert!(clipboard_matches("hello\n", "hello"));
        assert!(!clipboard_matches("hello", "hallo"));
        assert!(!clipboard_matches("hello", ""));
    }

    /// Live end-to-end smoke test for Wayland text injection.
    ///
    /// Deliberately excluded from normal runs (`#[ignore]`): it pastes into
    /// the currently focused window and only does so when every guard holds.
    ///
    /// Two supported targets, pick one and focus it:
    ///   1. Native Wayland terminal (foot): pastes text into a file.
    ///      swaymsg 'exec foot sh -c "cat > /tmp/voxtype-paste-smoke.txt"'
    ///   2. XWayland event tester (xev): verifies the X11 fallback path.
    ///      swaymsg 'exec sh -c "xev > /tmp/xev.log 2>&1"'
    ///
    /// Then run:
    ///   VOXTYPE_LIVE_SMOKE=1 cargo test -- --ignored live_wayland_paste_smoke --nocapture
    ///
    /// The test pastes ONLY when the focused window is a `foot` terminal or
    /// an XWayland window, so it can never type into an unrelated app.
    #[test]
    #[ignore]
    fn live_wayland_paste_smoke() {
        use std::io::Read as _;

        if std::env::var("VOXTYPE_LIVE_SMOKE").as_deref() != Ok("1") {
            eprintln!("skipping: VOXTYPE_LIVE_SMOKE=1 not set");
            return;
        }
        if detect_env() != DesktopEnv::Wayland
            || detect_wayland_compositor() != WaylandCompositor::Sway
        {
            panic!("live smoke test requires Sway on Wayland");
        }

        let marker = format!("voxtype smoke test {}", std::process::id());
        let target =
            detect_focus_target(WaylandCompositor::Sway).expect("no focused window detected");
        let is_foot = target.id.as_deref() == Some("foot");
        let is_xwayland = target.xwayland;
        assert!(
            is_foot || is_xwayland,
            "refusing to paste: focused window is {:?} (expected a scratch foot terminal or an XWayland window)",
            target
        );

        inject_text_wayland(&marker).expect("inject_text_wayland failed");

        if is_foot {
            // The scratch terminal runs `cat > /tmp/voxtype-paste-smoke.txt`;
            // the pasted text must land there.
            let path = "/tmp/voxtype-paste-smoke.txt";
            for _ in 0..50 {
                if let Ok(mut f) = std::fs::File::open(path) {
                    let mut s = String::new();
                    let _ = f.read_to_string(&mut s);
                    if s.trim() == marker {
                        let _ = std::fs::remove_file(path);
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let got = std::fs::read_to_string(path).unwrap_or_else(|_| "<unreadable>".into());
            panic!("paste did not land: file contains {:?}", got);
        } else {
            // XWayland target: the injector routes through the X11 path
            // (xdotool Ctrl+V). The focused xev window logs every KeyPress;
            // the paste shortcut must appear there.
            let path = "/tmp/xev.log";
            for _ in 0..50 {
                if let Ok(s) = std::fs::read_to_string(path) {
                    if s.contains("keysym")
                        && s.lines().filter(|l| l.contains("KeyPress")).count() > 0
                    {
                        let _ = std::fs::remove_file(path);
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let got = std::fs::read_to_string(path).unwrap_or_else(|_| "<unreadable>".into());
            panic!("paste shortcut did not reach xev; log contains {:?}", got);
        }
    }

    /// Live acceptance test: injection must stay bounded even when the paste
    /// tool hangs.
    ///
    /// Puts a fake `wtype` that sleeps forever on PATH, runs the real
    /// `inject_text_wayland`, and asserts it returns (with the clipboard
    /// fallback) well before a hung wtype would ever finish. This is the
    /// behavior that directly fixes the old multi-second stalls.
    #[test]
    #[ignore]
    fn live_wayland_bounded_smoke() {
        use std::os::unix::fs::PermissionsExt;

        if std::env::var("VOXTYPE_BOUNDED_SMOKE").as_deref() != Ok("1") {
            eprintln!("skipping: VOXTYPE_BOUNDED_SMOKE=1 not set");
            return;
        }
        if detect_env() != DesktopEnv::Wayland {
            panic!("bounded smoke test requires Wayland");
        }

        let fake_dir = format!("/tmp/voxtype-fake-bin-{}", std::process::id());
        let _ = std::fs::create_dir_all(&fake_dir);
        let fake_wtype = format!("{}/wtype", fake_dir);
        std::fs::write(&fake_wtype, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&fake_wtype, PermissionsExt::from_mode(0o755)).unwrap();

        let orig_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", fake_dir, orig_path));

        let t0 = std::time::Instant::now();
        let result = inject_text_wayland("bounded smoke test");
        let elapsed = t0.elapsed();
        let _ = std::fs::remove_dir_all(&fake_dir);
        std::env::set_var("PATH", orig_path);

        assert!(
            elapsed < Duration::from_secs(8),
            "injection took {:?} with a hanging wtype; expected a few seconds",
            elapsed
        );
        assert!(
            result.is_ok(),
            "injection should fall back gracefully: {:?}",
            result
        );
        eprintln!("bounded injection returned Ok in {:?}", elapsed);
    }

    /// Live end-user acceptance test: dictation text must actually land in a
    /// focused text field.
    ///
    /// Launches `zenity --entry` under XWayland (GDK_BACKEND=x11), focuses it,
    /// runs the real `inject_text_wayland` (which routes to the X11 injector),
    /// presses Return, and reads back what zenity's entry contained. This is
    /// the real acceptance path: the text the user dictated must appear in the
    /// field. Ignores any text already in the entry by clearing nothing; the
    /// marker is checked as a substring.
    #[test]
    #[ignore]
    fn live_xwayland_text_e2e() {
        if std::env::var("VOXTYPE_XWAYLAND_E2E").as_deref() != Ok("1") {
            eprintln!("skipping: VOXTYPE_XWAYLAND_E2E=1 not set");
            return;
        }
        if detect_env() != DesktopEnv::Wayland
            || detect_wayland_compositor() != WaylandCompositor::Sway
        {
            panic!("xwayland e2e test requires Sway on Wayland");
        }
        if !require_tool("zenity") || !require_tool("xdotool") {
            panic!("xwayland e2e test requires zenity and xdotool");
        }

        let out_path = format!("/tmp/voxtype-e2e-{}.txt", std::process::id());
        let out_file = std::fs::File::create(&out_path).unwrap();
        let mut zenity = Command::new("zenity")
            .env("GDK_BACKEND", "x11")
            .args(["--entry", "--title", "voxtype-e2e"])
            .stdout(std::process::Stdio::from(out_file))
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn zenity");

        // Focus the dialog and confirm it is the focused XWayland window.
        std::thread::sleep(Duration::from_millis(1500));
        let _ = Command::new("swaymsg")
            .args(["[title=voxtype-e2e]", "focus"])
            .output();
        std::thread::sleep(Duration::from_millis(300));
        let target =
            detect_focus_target(WaylandCompositor::Sway).expect("no focused window detected");
        assert!(
            target.xwayland,
            "expected an XWayland zenity dialog, got {:?}",
            target
        );

        let marker = format!("voxtype e2e marker {}", std::process::id());
        inject_text_wayland(&marker).expect("inject_text_wayland failed");

        // Press Return in the dialog: the default button confirms and zenity
        // prints the entry text to its stdout (our file).
        let _ = Command::new("xdotool").args(["key", "Return"]).output();

        let mut landed = false;
        for _ in 0..50 {
            if let Ok(Some(status)) = zenity.try_wait() {
                let got = std::fs::read_to_string(&out_path).unwrap_or_default();
                landed = status.success() && got.contains(&marker);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = zenity.kill();
        let got = std::fs::read_to_string(&out_path).unwrap_or_default();
        let _ = std::fs::remove_file(&out_path);
        assert!(
            landed,
            "dictated text did not land in the field; zenity output was {:?}",
            got
        );
        eprintln!("xwayland e2e: text landed in the field");
    }
}
