default:
    @just --list

build:
    cargo build --release --locked

check: build
    cargo fmt --check
    cargo clippy --release --locked --all-targets -- -D warnings
    cargo test --release --locked

clean:
    cargo clean
