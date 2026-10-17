//! Child processes the watchdogs may have to kill.

use std::process::{Child, Command, Stdio};

/// Kills `child` and every process it started (test binaries start isolated tests and cookd,
/// libFuzzer starts minimizing runs), so a stall leaves nothing running. Best effort: a
/// descendant whose parent already exited is no longer reachable from `child`.
pub fn kill_tree(child: &mut Child) {
    if cfg!(windows) {
        let _ = quiet("taskkill", &["/T", "/F", "/PID", &child.id().to_string()]);
    } else {
        // Children first, collected before any dies and is reparented.
        let mut tree = Vec::new();
        descendants(child.id(), &mut tree);
        for pid in tree.iter().rev() {
            let _ = quiet("kill", &["-KILL", &pid.to_string()]);
        }
    }
    let _ = child.kill();
}

/// The processes below `pid`, parents before their children (`pgrep` is on Linux and macOS).
fn descendants(pid: u32, out: &mut Vec<u32>) {
    let Ok(output) = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    for child in String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
    {
        out.push(child);
        descendants(child, out);
    }
}

fn quiet(program: &str, args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
}
