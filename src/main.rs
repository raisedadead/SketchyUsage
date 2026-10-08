use std::{process::ExitCode, thread};

use sketchyusage::{
    client,
    paths::Paths,
    server::{self, Start},
};

const USAGE: &str = "usage: sketchyusage <serve|toggle|push|--version>";

fn paths() -> Option<Paths> {
    match Paths::from_env() {
        Ok(paths) => Some(paths),
        Err(error) => {
            eprintln!("sketchyusage: {error}");
            None
        }
    }
}

fn serve() -> ExitCode {
    let Some(paths) = paths() else {
        return ExitCode::FAILURE;
    };
    match server::start(paths, Box::new(|| {})) {
        Ok(Start::AlreadyRunning) => {
            eprintln!("sketchyusage: another server holds the lock");
            ExitCode::SUCCESS
        }
        Ok(Start::Running(_server)) => loop {
            thread::park();
        },
        Err(error) => {
            eprintln!("sketchyusage: serve failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("sketchyusage {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("serve") => serve(),
        Some(command @ ("toggle" | "push")) => {
            if let Some(paths) = paths() {
                client::send_or_mark(&paths, command);
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(64)
        }
    }
}
