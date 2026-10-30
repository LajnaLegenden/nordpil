# List recipes
default:
    @just --list

# Run the app
run *args:
    cargo run -p nordpil -- {{args}}

# Format all code
fmt:
    cargo fmt --all

# Formatting, lints and unit tests. Run before every commit.
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Integration tests against a real mongod. Needs Docker.
test-int:
    cargo test --workspace -- --ignored
