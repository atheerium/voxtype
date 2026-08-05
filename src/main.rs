mod config;
mod dictation;

use anyhow::Result;
use std::process::Command;

use dictation::{daemon_pid, daemon_running};

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
    let flag = format!("-{}", signal);
    let pid_str = pid.to_string();
    let status = Command::new("kill")
        .args([flag.as_str(), pid_str.as_str()])
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to send {} to daemon: {}", signal, e))?;
    if !status.success() {
        // kill exits non-zero when the process is gone; give the user
        // feedback instead of silently swallowing the failed toggle.
        anyhow::bail!(
            "Failed to send {} to daemon (pid {}). Is voxtype still running?",
            signal,
            pid
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        // Print version and exit
        Some("--version") | Some("-V") => {
            println!("voxtype {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }

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
