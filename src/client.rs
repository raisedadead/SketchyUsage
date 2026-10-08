use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

use crate::{bar, paths::Paths};

pub const TIMEOUT: Duration = Duration::from_secs(2);

pub fn send(paths: &Paths, command: &str) -> bool {
    let Ok(mut stream) = UnixStream::connect(&paths.socket) else {
        return false;
    };
    if stream.set_read_timeout(Some(TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(TIMEOUT)).is_err()
    {
        return false;
    }
    if stream.write_all(format!("{command}\n").as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    stream.read_to_string(&mut reply).is_ok() && reply == "ok\n"
}

pub fn send_or_mark(paths: &Paths, command: &str) {
    if !send(paths, command) {
        eprintln!("sketchyusage: server unreachable; marking labels");
        bar::set_labels(bar::UNREACHABLE, bar::UNREACHABLE);
    }
}
