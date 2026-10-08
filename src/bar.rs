use std::{path::PathBuf, process::Command, time::Duration};

use crate::provider::run_with_timeout;

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const UNREACHABLE: &str = "— !";
pub const ITEMS: [&str; 2] = ["sketchyusage.claude", "sketchyusage.codex"];

fn binary() -> PathBuf {
    std::env::var_os("SKETCHYUSAGE_SKETCHYBAR")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || PathBuf::from("/opt/homebrew/bin/sketchybar"),
            PathBuf::from,
        )
}

pub fn set_labels(claude: &str, codex: &str) -> bool {
    let mut command = Command::new(binary());
    command.args([
        "--set",
        ITEMS[0],
        &format!("label={claude}"),
        "--set",
        ITEMS[1],
        &format!("label={codex}"),
    ]);
    run_with_timeout(&mut command, TIMEOUT).is_some_and(|output| output.status.success())
}

pub fn query(item: &str) -> Option<serde_json::Value> {
    let mut command = Command::new(binary());
    command.args(["--query", item]);
    let output =
        run_with_timeout(&mut command, TIMEOUT).filter(|output| output.status.success())?;
    serde_json::from_slice(&output.stdout).ok()
}
