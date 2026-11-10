#!/usr/bin/env bash
# Проверка APK на эмуляторе (GitHub Actions): ставит, запускает и смотрит,
# жива ли программа через 25 секунд. Журнал ошибок печатается в лог сборки.
set -uo pipefail
pkg=io.github.addfsw.poigrulkiny

adb install -r -g "${1:-Poigrulkiny-x86_64.apk}"
adb logcat -c
adb shell am start -n "$pkg/.MainActivity"
sleep 25

echo "===== logcat (программа, падения) ====="
adb logcat -d -v brief | grep -iE "$pkg|AndroidRuntime|FATAL|tauri|wry|Rust|libc\+\+|UnsatisfiedLink|DEBUG  " | tail -300
echo "===== конец logcat ====="

if adb shell pidof "$pkg" >/dev/null; then
  echo "Программа запустилась и работает."
  adb shell dumpsys activity activities | grep -E "mResumedActivity|topResumedActivity" | head -3
else
  echo "::error::Программа закрылась после запуска — причина в logcat выше."
  exit 1
fi
