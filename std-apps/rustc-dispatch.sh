#!/usr/bin/env bash
set -euo pipefail

HOST_RUSTC="$1"
shift

TARGET=""
PREV=""
for ARG in "$@"; do
    if [[ "$PREV" == "--target" ]]; then
        TARGET="$ARG"
        break
    fi
    case "$ARG" in
        --target=*)
            TARGET="${ARG#--target=}"
            break
            ;;
    esac
    PREV="$ARG"
done

if [[ "$TARGET" == "i686-unknown-popugos" ]]; then
    exec "${POPUGOS_RUSTC:?POPUGOS_RUSTC is not set}" "$@"
else
    exec "$HOST_RUSTC" "$@"
fi
