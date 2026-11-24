#!/bin/sh
set -eu

project_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
toolchain_dir=$(dirname -- "$project_dir")/toolchain
export CARGO_HOME="$toolchain_dir/cargo"
export RUSTUP_HOME="$toolchain_dir/rustup"
export CARGO_TARGET_DIR="$project_dir/apps/seller/target"
export MODELCAT_ZIG="$toolchain_dir/zig-aarch64-macos-0.15.2/zig"
export ZIG_GLOBAL_CACHE_DIR="$toolchain_dir/zig-cache"
export ZIG_LOCAL_CACHE_DIR="$toolchain_dir/zig-cache/local"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$project_dir/scripts/vm-cross-cc.sh"
export CC_aarch64_unknown_linux_gnu="$CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER"
export AR_aarch64_unknown_linux_gnu="$project_dir/scripts/vm-cross-ar.sh"

cd "$project_dir"
case "${1:-}:$#" in
    :0) guest=.vm-tools/guest ;;
    --oauth-preview:1) guest=.vm-tools/oauth-preview; set -- --features oauth-preview ;;
    *) exit 2 ;;
esac
"$CARGO_HOME/bin/cargo" build --locked --release \
    --target aarch64-unknown-linux-gnu --manifest-path apps/seller/Cargo.toml "$@"
mkdir -p "$guest"
staged=$(mktemp "$guest/modelcat.XXXXXXXX")
trap 'rm -f "$staged"' EXIT
install -m 755 "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/modelcat" "$staged"
mv -f "$staged" "$guest/modelcat"
