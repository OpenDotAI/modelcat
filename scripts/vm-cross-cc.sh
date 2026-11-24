#!/bin/bash
args=()
for arg in "$@"; do
    case "$arg" in
        --target=aarch64-unknown-linux-gnu|-Wl,--fix-cortex-a53-843419) ;;
        *) args+=("$arg") ;;
    esac
done
exec "$MODELCAT_ZIG" cc -target aarch64-linux-gnu.2.39 "${args[@]}"
