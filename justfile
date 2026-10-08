default:
    @just --list

build:
    cargo build --release --locked

check: build
    cargo fmt --check
    cargo clippy --release --locked --all-targets -- -D warnings
    cargo test --release --locked

install prefix=(home_directory() / ".local"): build
    install -d "{{ prefix }}/bin"
    install -m 755 target/release/sketchyusage "{{ prefix }}/bin/sketchyusage"

clean:
    cargo clean
