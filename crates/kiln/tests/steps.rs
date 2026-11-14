use std::process::Command;
use std::time::{Duration, Instant};

use kiln_runner::{Marker, parse_marker};

fn run_steps(steps: serde_json::Value, timeout: u64) -> (Option<i32>, Vec<Marker>, String) {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_kiln"))
        .args(["helper", "steps"])
        .current_dir(dir.path())
        .env("KILN_STEPS", steps.to_string())
        .env("KILN_TIMEOUT", timeout.to_string())
        .env("NPM_TOKEN", "npm_s3cr3t_value")
        .env("KILN_SECRET_NAMES", "NPM_TOKEN")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let markers = stdout.lines().filter_map(parse_marker).collect();
    (output.status.code(), markers, stdout)
}

fn exits(markers: &[Marker]) -> Vec<(usize, i32)> {
    markers
        .iter()
        .filter_map(|m| match m {
            Marker::StepEnd { index, exit, .. } => Some((*index, *exit)),
            _ => None,
        })
        .collect()
}

#[test]
fn steps_share_the_workspace_and_stop_at_the_first_failure() {
    let (code, markers, stdout) = run_steps(
        serde_json::json!([
            { "name": "write", "run": "echo hello > note.txt; echo token=$NPM_TOKEN; echo $NPM_TOKEN | base64 >&2" },
            { "name": "read", "run": "cat note.txt" },
            { "name": "pipe", "run": "false | cat\necho unreachable" },
            { "name": "never", "run": "echo never" },
        ]),
        60,
    );
    assert_eq!(code, Some(1));
    assert_eq!(exits(&markers), [(0, 0), (1, 0), (2, 1)]);
    assert!(stdout.contains("hello"));
    assert!(stdout.contains("token=***"), "{stdout}");
    assert!(!stdout.contains("npm_s3cr3t_value"), "{stdout}");
    assert!(!stdout.lines().any(|l| l == "unreachable"), "pipefail stops the step: {stdout}");
    assert!(!stdout.lines().any(|l| l == "never"));
}

#[test]
fn timeouts_stop_the_whole_process_group() {
    let started = Instant::now();
    let (code, markers, stdout) =
        run_steps(serde_json::json!([{ "name": "hang", "run": "sleep 30 & sleep 30; wait" }]), 1);
    assert!(started.elapsed() < Duration::from_secs(15), "took {:?}", started.elapsed());
    assert_eq!(code, Some(124));
    assert_eq!(exits(&markers), [(0, 124)]);
    assert!(stdout.contains("timeout"), "{stdout}");
}
