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

# Render UI screenshots to target/screenshots for a visual check
screenshots:
    cargo test -p nordpil --features screenshots -- --ignored screenshot

# Build a release and install it with a launcher entry and icon (Linux, uses sudo)
install prefix="/usr/local":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$(uname -s)" != Linux ]; then
        echo "just install supports Linux only for now." >&2
        exit 1
    fi
    cargo build --release --locked -p nordpil
    sed 's|^Exec=.*|Exec={{prefix}}/bin/nordpil|' crates/app/assets/nordpil.desktop \
        > target/release/nordpil.desktop
    sudo install -Dm755 target/release/nordpil "{{prefix}}/bin/nordpil"
    sudo install -Dm644 target/release/nordpil.desktop "{{prefix}}/share/applications/nordpil.desktop"
    sudo install -Dm644 crates/app/assets/nordpil.svg "{{prefix}}/share/icons/hicolor/scalable/apps/nordpil.svg"
    echo "Installed nordpil to {{prefix}}."

# Remove what `just install` put in place. Saved connections and settings stay.
uninstall prefix="/usr/local":
    sudo rm -f "{{prefix}}/bin/nordpil" \
        "{{prefix}}/share/applications/nordpil.desktop" \
        "{{prefix}}/share/icons/hicolor/scalable/apps/nordpil.svg"
