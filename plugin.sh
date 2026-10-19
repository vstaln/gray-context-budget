#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/context-budget.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/context-budget" ;;
    *) rel="x86_64-apple-darwin/context-budget" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/context-budget" ;;
    *) rel="x86_64-unknown-linux-musl/context-budget" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/context-budget.exe" "$here/target/release/context-budget" "$here/target/debug/context-budget.exe" "$here/target/debug/context-budget"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "context-budget binary not found in $here" >&2
exit 1
