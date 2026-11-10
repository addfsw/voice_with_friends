#!/usr/bin/env bash
# Проверяет, что код собирается под Android, без Android NDK и SDK:
# `cargo check`/`cargo clippy` под aarch64-linux-android.
#
# C-библиотеки (ring, OpenH264, Opus) при проверке не нужны: вместо
# компилятора — заглушка, которая создаёт пустые объектные файлы, вместо
# CMake — заглушка, которая ничего не делает. Сама сборка APK — в GitHub
# Actions (.github/workflows/android.yml).
#
#   scripts/android-check.sh            # cargo check
#   scripts/android-check.sh clippy     # cargo clippy
set -euo pipefail

cd "$(dirname "$0")/.."
rustup target add aarch64-linux-android >/dev/null

fake="$(mktemp -d)"
trap 'rm -rf "$fake"' EXIT
# Имя «clang»: по нему cc-rs понимает, какие флаги передавать.
cat > "$fake/clang" <<'SH'
#!/bin/sh
for a in "$@"; do
  case "$a" in
    -E|"-?") exit 1 ;;
    --version|-dumpversion) echo "clang version 17.0.0"; exit 0 ;;
  esac
done
out=""; prev=""
for a in "$@"; do
  [ "$prev" = "-o" ] && out="$a"
  prev="$a"
done
[ -n "$out" ] && : > "$out"
exit 0
SH
printf '#!/bin/sh\nexit 0\n' > "$fake/cmake"
chmod +x "$fake/clang" "$fake/cmake"

export CC_aarch64_linux_android="$fake/clang"
export CXX_aarch64_linux_android="$fake/clang"
export AR_aarch64_linux_android=ar
export CMAKE="$fake/cmake"
export CARGO_TARGET_DIR=target-android
cargo "${1:-check}" --target aarch64-linux-android -p vwf-app
