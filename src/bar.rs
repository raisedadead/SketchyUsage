use std::{path::PathBuf, process::Command, time::Duration};

use crate::provider::run_with_timeout;

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const UNREACHABLE: &str = "— !";

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
        "usage.claude",
        &format!("label={claude}"),
        "--set",
        "usage.codex",
        &format!("label={codex}"),
    ]);
    run_with_timeout(&mut command, TIMEOUT).is_some_and(|output| output.status.success())
}
