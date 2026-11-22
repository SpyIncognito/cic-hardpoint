#!/usr/bin/env bash
# The Windows release exe, with the build machine's paths (the checkout, the cargo registry and
# git checkouts) remapped, so none of them end up in the exe's panic messages.
# Usage: tools/build-release.sh   (writes target/x86_64-pc-windows-msvc/release/hardpoint.exe)
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
cd "$root"
# RUSTFLAGS replaces the target's rustflags in .cargo/config.toml, so the static C runtime is
# asked for here too.
export RUSTFLAGS="-C target-feature=+crt-static --remap-path-prefix=$root=/hardpoint --remap-path-prefix=$cargo_home/registry/src=/cargo/registry --remap-path-prefix=$cargo_home/git/checkouts=/cargo/git --remap-path-prefix=$HOME=/build"
cargo xwin build --release --target x86_64-pc-windows-msvc -p sc-app
exe=target/x86_64-pc-windows-msvc/release/hardpoint.exe
echo "$exe $(stat -c %s "$exe") bytes, sha256 $(sha256sum "$exe" | cut -d' ' -f1)"
