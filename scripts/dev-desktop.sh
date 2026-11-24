#!/bin/sh
set -eu

project_dir=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
dev_dir=$(dirname -- "$project_dir")
export CARGO_HOME="$dev_dir/toolchain/cargo"
export RUSTUP_HOME="$dev_dir/toolchain/rustup"
export CARGO_TARGET_DIR="$project_dir/apps/seller/target"
export MODELCAT_VM_RUNTIME="$project_dir/.vm-tools"
export MODELCAT_VM_DEV_DATA="${MODELCAT_VM_DEV_DATA:-$dev_dir/ModelCatVMData}"

case "${1:-}:$#" in
    :0) ;;
    --oauth-preview:1)
        if [ ! -x '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome' ]; then
            printf '%s\n' '模拟授权需要 /Applications 中的 Google Chrome。' >&2
            exit 1
        fi
        export MODELCAT_OAUTH_PREVIEW=1
        ;;
    --oauth-preview-proxy:1)
        workspace="$MODELCAT_VM_DEV_DATA"
        export HOME="$workspace/host-home" LIMA_HOME="$workspace/lima"
        export TMPDIR="$workspace/tmp" XDG_CACHE_HOME="$workspace/cache"
        cd /
        fixture=$(base64 < "$project_dir/scripts/oauth-preview-proxy.py" | tr -d '\n')
        exec ssh -tt -F "$LIMA_HOME/modelcat/ssh.config" \
            -o ControlPath=none -o ExitOnForwardFailure=yes \
            -L 127.0.0.1:18779:127.0.0.1:18779 lima-modelcat \
            "exec python3 -u -c 'import base64; exec(base64.b64decode(\"$fixture\"))'"
        ;;
    --stop:1)
        exec "$CARGO_TARGET_DIR/debug/modelcat" workspace-stop
        ;;
    upstream:*|network:*|tool:*|api-key:1|status:1|bind:*|config:*|quotes:1|probe:*|pause:1|resume:1)
        exec "$CARGO_TARGET_DIR/debug/modelcat" "$@"
        ;;
    *)
        printf 'Usage: %s [--oauth-preview | --oauth-preview-proxy | --stop | MODEL_CAT_COMMAND]\n' "$0" >&2
        exit 2
        ;;
esac

cd "$project_dir"
"$CARGO_HOME/bin/cargo" build --locked --features desktop --manifest-path apps/seller/Cargo.toml
python3 scripts/prepare-vm-runtime.py
"$project_dir/scripts/build-vm-backend.sh" "$@"

app="$CARGO_TARGET_DIR/debug/ModelCat Dev.app"
contents="$app/Contents"
mkdir -p "$contents/MacOS" "$contents/Resources"
cp apps/seller/icons/ModelCat.icns "$contents/Resources/ModelCat.icns"
plist="$contents/Info.plist"
plutil -create xml1 "$plist"
plutil -insert CFBundleIdentifier -string ai.opendot.modelcat.dev "$plist"
plutil -insert CFBundleName -string 'ModelCat Dev' "$plist"
plutil -insert CFBundleExecutable -string modelcat "$plist"
plutil -insert CFBundlePackageType -string APPL "$plist"
plutil -insert CFBundleIconFile -string ModelCat.icns "$plist"
ln -f "$CARGO_TARGET_DIR/debug/modelcat" "$contents/MacOS/modelcat"

# Keep the preview attached to the invoking terminal and stream its logs there.
exec "$contents/MacOS/modelcat"
