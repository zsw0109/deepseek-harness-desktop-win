//! DeepSeek Harness service lifecycle management.
//!
//! The embedded browser needs the DSH Web GUI on port 3080. This module
//! detects the process listening on that port, restarts the service via
//! `npx @deepseek-ai/dsh web`, waits for it to come up, and kills it when
//! the desktop app exits.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Port used by the DeepSeek Harness Web GUI.
pub const DSH_PORT: u16 = 3080;

/// Hide the child's console window (this is a GUI app).
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// PIDs of processes currently LISTENING on the given TCP port.
///
/// Uses `netstat -ano` (the data columns are stable and locale-independent;
/// only the header text is localized). The PID is the last token of a row.
pub fn pids_listening_on(port: u16) -> Vec<u32> {
    let output = match Command::new("netstat")
        .args(["-ano"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => out.stdout,
        _ => return Vec::new(),
    };

    let text = String::from_utf8_lossy(&output);
    let local_suffix = format!(":{port}");
    let mut pids = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let parts: Vec<&str> = line.split_whitespace().collect();
        // TCP rows: [Proto, Local, Foreign, State, PID]
        if parts.len() < 5 || parts[0] != "TCP" || parts[3] != "LISTENING" {
            continue;
        }
        if parts[1].ends_with(&local_suffix) {
            if let Ok(pid) = parts[4].parse::<u32>() {
                pids.push(pid);
            }
        }
    }
    pids
}

/// Is anything listening on the port right now?
pub fn is_port_listening(port: u16) -> bool {
    !pids_listening_on(port).is_empty()
}

/// Kill a process and its whole child tree (`taskkill /T /F`).
pub fn kill_pid_tree(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Kill every process listening on the port (including their trees).
pub fn kill_port_owner(port: u16) {
    for pid in pids_listening_on(port) {
        kill_pid_tree(pid);
    }
}

/// Wait until the port is no longer listening (the old process released it).
pub fn wait_port_closed(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_port_listening(port) {
            return true;
        }
        thread::sleep(Duration::from_millis(300));
    }
    !is_port_listening(port)
}

/// Start `npx @deepseek-ai/dsh web` with a hidden console.
///
/// Returns the child process PID on success. `npx` is a `.cmd` shim on
/// Windows, so it is invoked through `cmd /C`. `--yes` keeps it
/// non-interactive (a hidden child must never wait on a prompt).
pub fn start_dsh() -> Option<u32> {
    let child = Command::new("cmd")
        .args(["/C", "npx --yes @deepseek-ai/dsh web"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    Some(child.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The port lookup must find the DSH listener when the service is running.
    #[test]
    fn detects_listener_on_dsh_port() {
        let pids = pids_listening_on(DSH_PORT);
        eprintln!("listeners on port {DSH_PORT}: {pids:?}");
        if pids.is_empty() {
            eprintln!("note: no listener on port {DSH_PORT} (DSH not running?) - skipping");
            return;
        }
        assert!(pids.iter().all(|pid| *pid > 0));
    }

    /// `cmd /C npx ...` must be able to resolve the npx.cmd shim.
    #[test]
    fn npx_resolves_via_cmd() {
        let output = Command::new("cmd")
            .args(["/C", "npx --version"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .expect("spawn cmd");
        assert!(output.status.success(), "npx did not resolve: {:?}", output);
        eprintln!("npx version: {}", String::from_utf8_lossy(&output.stdout).trim());
    }

    /// `taskkill /T /F` must actually terminate a spawned process tree.
    ///
    /// Ignored by default: this DSH file sandbox denies process termination
    /// ("Access denied"), so it only passes outside a sandboxed environment.
    #[test]
    #[ignore = "requires an unsandboxed environment (taskkill is blocked here)"]
    fn kills_process_tree() {
        let mut child = Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1"])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("spawn dummy process");
        let pid = child.id();
        thread::sleep(Duration::from_millis(300));
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "dummy process {pid} should be running"
        );

        kill_pid_tree(pid);
        thread::sleep(Duration::from_millis(400));
        assert!(
            child.try_wait().expect("try_wait").is_some(),
            "process {pid} should have been killed by taskkill"
        );
    }
}
