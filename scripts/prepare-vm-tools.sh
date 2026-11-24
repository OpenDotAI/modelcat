#!/bin/sh
set -eu
if [ -n "${MODELCAT_TOOL_DATA:-}" ]; then
    output="$MODELCAT_TOOL_DATA"
else
    root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
    output="$root/.vm-tools/tools"
fi

case "${1:-}" in
    claude)
        tool=claude
        url=https://registry.npmjs.org/@anthropic-ai/claude-code-linux-arm64/-/claude-code-linux-arm64-2.1.329.tgz
        hash=IiG5umFToSCVWqB1lEsxxwIxKgxPXyZ3DRU915v8J4Bz5Q3JpslM0ew1m5yaT+lFXBPvYxmsEgi0XI68XHND0A==
        ;;
    codex)
        tool=codex
        url=https://registry.npmjs.org/@openai/codex/-/codex-0.171.0-linux-arm64.tgz
        hash=9z17t4bbUJ9UCIb49O4PoA+ki+45q3rd8nZ8PeH6sYnlKkBld/Xpo/OS3h3Ti954+foshX4o7k5TCU8N7zzqQA==
        ;;
    *) printf '%s\n' 'Usage: prepare-vm-tools.sh claude|codex' >&2; exit 2 ;;
esac

mkdir -p "$output"
stage=$(mktemp -d "$output/.prepare.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT

fetch() {
    curl -q -fsSL --proto '=https' --proto-redir '=https' --retry 2 \
        --connect-timeout 15 --max-time 180 -o "$2" "$1"
}

base="$stage/ubuntu-base.tar.gz"
base_hash=a91d5a93010193712d346d761372b7c9db6dfcf093893161c64ca107f05914f2
if [ ! -f "$output/ubuntu-base.tar.gz" ]; then
    fetch https://cdimage.ubuntu.com/ubuntu-base/releases/24.04/release/ubuntu-base-24.04.5-base-arm64.tar.gz "$base"
    test "$(shasum -a 256 "$base" | awk '{print $1}')" = "$base_hash"
    mv -f "$base" "$output/ubuntu-base.tar.gz"
fi

receipt="$output/.$tool.integrity"
if [ -x "$output/$tool" ] && [ -f "$receipt" ] && [ "$(cat "$receipt")" = "$hash" ]; then
    exit 0
fi
archive="$stage/$tool.tar.gz"
fetch "$url" "$archive"
test "$(openssl dgst -sha512 -binary "$archive" | openssl base64 -A)" = "$hash"
tar -xzf "$archive" -C "$stage"
binary=$(find "$stage/package" -type f \( -name "$tool" -o -name "$tool.exe" \) -print)
test -f "$binary"
install -m 755 "$binary" "$stage/program"
mv -f "$stage/program" "$output/$tool"
printf '%s\n' "$hash" > "$stage/integrity"
mv -f "$stage/integrity" "$receipt"
