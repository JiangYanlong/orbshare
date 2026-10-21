#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-run}"
APP_NAME="OrbShare"
BUNDLE_ID="com.orbshare.desktop"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_BUNDLE="$ROOT_DIR/target/debug/bundle/macos/$APP_NAME.app"
APP_PROCESS="orbshare"

cd "$ROOT_DIR"

stop_app() {
  pkill -f "$APP_BUNDLE/Contents/MacOS/$APP_PROCESS" >/dev/null 2>&1 || true
  pkill -x "$APP_PROCESS" >/dev/null 2>&1 || true
  for _ in 1 2 3 4 5; do
    pgrep -x "$APP_PROCESS" >/dev/null 2>&1 || return 0
    sleep 0.2
  done
}

build_app() {
  npm run build
  npm run tauri -- build --debug --bundles app
  "$ROOT_DIR/script/sign_macos_app.sh" debug
}

open_app() {
  # The build can take a while. Stop again here so an obsolete bundle opened
  # during compilation cannot keep owning the shared Bundle ID in LaunchServices.
  stop_app
  /usr/bin/open -n "$APP_BUNDLE"
}

case "$MODE" in
  run)
    stop_app
    build_app
    open_app
    ;;
  --debug|debug)
    stop_app
    build_app
    lldb -- "$ROOT_DIR/target/debug/$APP_PROCESS"
    ;;
  --logs|logs)
    stop_app
    build_app
    open_app
    /usr/bin/log stream --info --style compact --predicate "process == \"$APP_PROCESS\""
    ;;
  --telemetry|telemetry)
    stop_app
    build_app
    open_app
    /usr/bin/log stream --info --style compact --predicate "subsystem == \"$BUNDLE_ID\""
    ;;
  --verify|verify)
    stop_app
    build_app
    open_app
    sleep 2
    pgrep -f "$APP_BUNDLE/Contents/MacOS/$APP_PROCESS" >/dev/null
    ;;
  --reset-permission|reset-permission)
    stop_app
    build_app
    # Remove only OrbShare's stale screen-capture entries. The newly opened,
    # stably signed app will appear once and can then retain the permission.
    /usr/bin/tccutil reset ScreenCapture "$BUNDLE_ID"
    open_app
    /usr/bin/open "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
    echo "请在系统设置中只启用当前 OrbShare，然后完全退出并重新打开一次。"
    ;;
  *)
    echo "usage: $0 [run|--debug|--logs|--telemetry|--verify|--reset-permission]" >&2
    exit 2
    ;;
esac
