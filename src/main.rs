mod config;
mod dictation;
mod stats;
mod tui;

use anyhow::Result;
use std::process::Command;

use dictation::{daemon_pid, daemon_running, process_alive};

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
            "Failed to send {} to daemon (pid {}). Is libretype still running?",
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
        // Print help
        Some("--help") | Some("-h") => {
            println!(
                "libretype {} — voice-to-text dictation for Linux\\n",
                env!("CARGO_PKG_VERSION")
            );
            println!("USAGE:");
            println!("  libretype            Toggle recording (Ctrl+Space via daemon)");
            println!("  libretype --daemon   Start daemon in background");
            println!("  libretype --restart  Restart daemon (picks up new binary)");
            println!("  libretype --stats    Show provider usage statistics");
            println!("  libretype --configure  Interactively set default STT provider");
            println!("  libretype --version  Print version");
            return Ok(());
        }

        // Print version and exit
        Some("--version") | Some("-V") => {
            println!("libretype {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }

        // Show provider statistics
        Some("--stats") | Some("stats") => {
            stats::Stats::load().print_table();
            return Ok(());
        }

        // Interactive configuration
        Some("--configure") | Some("configure") => {
            return tui::run_configure();
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

        // Restart daemon: kill existing and spawn fresh (picks up new binary)
        Some("--restart") | Some("restart") => {
            if let Some(pid) = daemon_pid() {
                send_signal("TERM", pid)?;
                // Wait for process to exit
                for _ in 0..50 {
                    if !process_alive(pid) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            spawn_daemon()?;
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
