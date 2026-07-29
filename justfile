_default:
    @just --list

alias b  := build
alias br := build-release
alias r  := run
alias rr := run-release
alias c  := check
alias t  := test

build:
    cargo build

build-release:
    cargo build --release

run *args:
    cargo run -- {{args}}

run-release *args:
    cargo run --release -- {{args}}

check:
    cargo check

test:
    cargo test

clean:
    cargo clean

update:
    cargo update

fmt:
    cargo +nightly fmt

lint:
    cargo clippy -- -D warnings

lint-fix:
    cargo clippy --fix --allow-dirty --allow-staged

install:
    cargo install --path . --force
    @echo "Peep installed system-wide!"

uninstall:
    cargo uninstall peep
    @echo "Peep uninstalled."
