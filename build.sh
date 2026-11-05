#!/bin/sh
# Builds IMVUWU on Linux or macOS with the normal Rust toolchain (rustup or
# your distro's cargo). Windows uses build.ps1; NixOS can use `nix build`.
#
#   ./build.sh          release build into dist/:
#                         IMVUWU-<version>-<os>-<arch>-no-imvu-avatars[.tar.gz]
#                           (downloads IMVU's avatars on first launch)
#                         IMVUWU-<version>-<os>-<arch>, only where the
#                           developer's IMVU avatar files exist (not published)
#   ./build.sh test     tests and clippy
#
# Linux needs a C compiler (for the ring TLS library) and, to run the app,
# the usual desktop libraries: OpenGL/EGL, libxkbcommon, Wayland or X11,
# which every desktop install already has. Debian/Ubuntu build packages:
#   sudo apt install build-essential pkg-config
set -eu
cd "$(dirname "$0")"

if [ "${1:-}" = "test" ]; then
    cargo test --locked
    cargo clippy --locked --release --all-targets -- -D warnings
    if [ -d assets/poser/female ]; then
        cargo test --locked --features bundled-imvu-avatars --target-dir target/release-avatars
    fi
    exit 0
fi

cargo build --release --locked

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
share="IMVUWU-$version-$os-$arch-no-imvu-avatars"
mkdir -p dist
cp target/release/headmaker "dist/$share"
chmod +x "dist/$share"
# The shared build as a .tar.gz holding "IMVUWU" (downloads drop the
# executable bit; tar keeps it).
staging=$(mktemp -d)
cp "dist/$share" "$staging/IMVUWU"
chmod 755 "$staging/IMVUWU"
tar -czf "dist/$share.tar.gz" -C "$staging" IMVUWU
rm -rf "$staging"
echo "Built dist/$share and dist/$share.tar.gz"

# The developer's IMVU avatar files are not in the public repository.
if [ -d assets/poser/female ]; then
    cargo build --release --locked --features bundled-imvu-avatars --target-dir target/release-avatars
    cp target/release-avatars/release/headmaker "dist/IMVUWU-$version-$os-$arch"
    chmod +x "dist/IMVUWU-$version-$os-$arch"
    echo "Built dist/IMVUWU-$version-$os-$arch (embedded avatars)"
fi
