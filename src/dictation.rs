use anyhow::{Context, Result};
use reqwest::multipart;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tokio::process::Command as TokioCommand;
use tokio::signal::unix::{signal, SignalKind};

use crate::config::Config;

// ── File paths ────────────────────────────────────────────────────

const PIDFILE: &str = "/tmp/voxtype.pid";
const LOCKFILE: &str = "/tmp/voxtype.lock";
const AUDIO_FILE: &str = "/tmp/voxtype.mp3";

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
    let ms = d.subsec_millis();
    // Simple ISO-like format: hour:minute:second
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
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
        // SIGTERM first, then SIGKILL if still alive
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
    for tool in &["ffmpeg", "xdotool", "xsel", "xclip"] {
        if !require_tool(tool) {
            missing.push(tool.to_string());
        }
    }
    missing
}

// ── Daemon ────────────────────────────────────────────────────────

pub async fn run_daemon() -> Result<()> {
    // Write PID file
    fs::write(PIDFILE, std::process::id().to_string())
        .context("Failed to write PID file")?;

    // Validate system dependencies
    let missing = validate_deps();
    if !missing.is_empty() {
        let msg = format!(
            "Missing runtime dependencies: {}. Install with: sudo apt install ffmpeg xdotool xsel xclip",
            missing.join(", ")
        );
        write_log(&msg);
        eprintln!("{}", msg);
    }

    write_log(&format!(
        "Daemon started. DISPLAY={:?}, XAUTHORITY={:?}",
        std::env::var("DISPLAY").unwrap_or_default(),
        std::env::var("XAUTHORITY").unwrap_or_default()
    ));

    // Signal handlers
    let mut usr1 = signal(SignalKind::user_defined1())
        .context("Failed to setup SIGUSR1 handler")?;
    let mut term = signal(SignalKind::terminate())
        .context("Failed to setup SIGTERM handler")?;
    let mut int = signal(SignalKind::interrupt())
        .context("Failed to setup SIGINT handler")?;

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

// ── Toggle ────────────────────────────────────────────────────────

async fn toggle() {
    if is_recording() {
        match stop_and_transcribe().await {
            Ok(text) => write_log(&format!("Transcribed and injected: {} chars", text.len())),
            Err(e) => {
                let msg = format!("Transcription/paste failed: {}", e);
                write_log(&msg);
                eprintln!("{}", msg);
            }
        }
    } else {
        match start_recording() {
            Ok(()) => write_log("Recording started"),
            Err(e) => {
                let msg = format!("Recording failed: {}", e);
                write_log(&msg);
                eprintln!("{}", msg);
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
        anyhow::bail!("ffmpeg not found. Install it: sudo apt install ffmpeg");
    }

    // Check PulseAudio source
    let pa_check = Command::new("pactl")
        .args(["info"])
        .output();
    if let Err(e) = pa_check {
        anyhow::bail!("PulseAudio not running. Start PulseAudio first. ({})", e);
    }

    let child = TokioCommand::new("ffmpeg")
        .args([
            "-y",
            "-f", "pulse",
            "-i", "default",
            "-ac", "1",
            "-ar", "16000",
            "-b:a", "64k",
            "-loglevel", "error",
            AUDIO_FILE,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to start ffmpeg recording")?;

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
        anyhow::bail!("Audio file too small ({} bytes). Check microphone input.", meta.len());
    }

    let config = Config::load()?;
    let api_key = config.groq_api_key()?;

    let text = transcribe(&api_key, &config).await?;

    if text.trim().is_empty() {
        let _ = fs::remove_file(AUDIO_FILE);
        anyhow::bail!("Transcription returned empty result");
    }

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
        .context("Failed to reach Groq API (check network)")?;

    let status = response.status();
    let body = response.text().await
        .context("Failed to read API response")?;

    if !status.is_success() {
        anyhow::bail!("Groq API error (HTTP {}): {}", status, body);
    }

    // response_format=text returns raw text, not JSON
    // But we also handle JSON format for safety
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(text) = json["text"].as_str() {
            return Ok(text.to_string());
        }
    }

    // Raw text response
    let trimmed = body.trim();
    if !trimmed.is_empty() {
        return Ok(trimmed.to_string());
    }

    anyhow::bail!("Empty response from API");
}

// ── X11 Text Injection ────────────────────────────────────────────

fn inject_text(text: &str) -> Result<()> {
    // 1. Set clipboard via xsel
    set_clipboard_xsel(text)?;

    // 2. Also set via xclip as fallback
    let _ = set_clipboard_xclip(text);

    // 3. Wait for clipboard propagation
    std::thread::sleep(Duration::from_millis(100));

    // 4. Detect active window type
    let is_term = is_terminal_window();

    write_log(&format!(
        "Injecting {} chars into {} window",
        text.len(),
        if is_term { "terminal" } else { "GUI" }
    ));

    // 5. Simulate paste with the correct shortcut
    if is_term {
        // Terminals typically use Ctrl+Shift+V for paste
        Command::new("xdotool")
            .args(["key", "ctrl+shift+v"])
            .output()
            .context("xdotool key ctrl+shift+v failed")?;
    } else {
        // GUI apps typically use Ctrl+V
        Command::new("xdotool")
            .args(["key", "ctrl+v"])
            .output()
            .context("xdotool key ctrl+v failed")?;
    }

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
        // stdin is dropped here, closing the pipe
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

fn is_terminal_window() -> bool {
    // Step 1: Get active window ID
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
        write_log("xdotool returned empty window ID");
        return false;
    }

    // Step 2: Get WM_CLASS via xprop (reliable on all X11)
    let xprop_out = Command::new("xprop")
        .args(["-id", &winid, "WM_CLASS"])
        .output();

    if let Ok(out) = xprop_out {
        let stdout = String::from_utf8_lossy(&out.stdout);
        // WM_CLASS(STRING) = "instance", "class"
        // Extract the second quoted string (class)
        if let Some(second_quote) = stdout.rsplit('"').nth(1) {
            let class = second_quote.trim().to_lowercase();
            let known_terminals = [
                "alacritty", "xfce4-terminal", "xfterminal", "gnome-terminal",
                "gnome-terminal-server", "konsole", "xterm", "uxterm",
                "urxvt", "urxvtc", "terminator", "tilix", "kitty",
                "wezterm", "wezterm", "st", "st-256color", "rxvt",
                "guake", "mate-terminal", "lxterminal", "cool-retro-term",
                "deepin-terminal", "sakura", "termite",
            ];
            let is_term = known_terminals.contains(&class.as_str());
            write_log(&format!(
                "Window WM_CLASS: '{}' -> {}",
                class,
                if is_term { "terminal" } else { "not terminal" }
            ));
            if is_term {
                return true;
            }
        } else {
            write_log(&format!("Could not parse WM_CLASS from: {}", stdout.trim()));
        }
    } else {
        write_log("xprop not available, falling back to PID detection");
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
                let known_procs = [
                    "alacritty", "xfce4-terminal", "gnome-terminal", "gnome-terminal-server",
                    "konsole", "xterm", "urxvt", "urxvtc", "terminator", "tilix",
                    "kitty", "wezterm", "st", "guake", "mate-terminal",
                    "lxterminal", "sakura", "termite",
                ];
                let is_term = known_procs.contains(&proc_name.as_str());
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
