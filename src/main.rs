mod config;
mod dictation;

use anyhow::Result;
use std::fs;
use std::process::Command;

const PIDFILE: &str = "/tmp/voxtype.pid";

fn daemon_running() -> bool {
    if let Ok(content) = fs::read_to_string(PIDFILE) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            return Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
        }
    }
    false
}

fn daemon_pid() -> Option<u32> {
    fs::read_to_string(PIDFILE).ok().and_then(|c| c.trim().parse::<u32>().ok())
}

fn spawn_daemon() -> Result<()> {
    Command::new(std::env::current_exe()?)
        .arg("__daemon")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn daemon: {}", e))?;

    // Wait for daemon to initialize and write PID
    for _ in 0..20 {
        if daemon_running() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::bail!("Daemon failed to start within 2 seconds");
}

fn send_signal(signal: &str, pid: u32) -> Result<()> {
    Command::new("kill")
        .args([format!("-{}", signal).as_str(), &pid.to_string()])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to send {} to daemon: {}", signal, e))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        // Internal: run as persistent daemon
        Some("__daemon") => return dictation::run_daemon().await,

        // User request: start daemon silently (no toggle)
        Some("--daemon") | Some("-d") => {
            if !daemon_running() {
                spawn_daemon()?;
            }
            return Ok(());
        }

        // Default: toggle recording via SIGUSR1
        _ => {
            if !daemon_running() {
                spawn_daemon()?;
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            if let Some(pid) = daemon_pid() {
                send_signal("USR1", pid)?;
            } else {
                anyhow::bail!("Daemon not running");
            }
        }
    }

    Ok(())
}
