#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="${1:-release}"
APP="$ROOT/target/$CONFIG/bundle/macos/OrbShare.app"
BUNDLE_ID="com.orbshare.desktop"
ENTITLEMENTS="$ROOT/src-tauri/Entitlements.plist"

if [[ ! -d "$APP" ]]; then
  echo "OrbShare.app not found: $APP" >&2
  echo "Build it first with: npm run tauri build${CONFIG/debug/ -- --debug}" >&2
  exit 1
fi

# Tauri's plain ad-hoc signature uses the changing binary CDHash as its
# designated requirement. macOS TCC then treats every rebuild as a different
# screen-recording client. Keep local builds ad-hoc, but embed one stable local
# designated requirement so permission survives rebuilds on the same Mac.
# Production/App Store builds must replace this with an Apple certificate.
codesign --force --deep --sign - \
  --identifier "$BUNDLE_ID" \
  --options runtime \
  --entitlements "$ENTITLEMENTS" \
  --requirements "=designated => identifier \"$BUNDLE_ID\"" \
  "$APP"

codesign --verify --deep --strict --verbose=2 "$APP"
codesign -d -r- "$APP" 2>&1 | grep -F "designated => identifier \"$BUNDLE_ID\""
codesign -d --entitlements :- "$APP" 2>/dev/null | grep -F "com.apple.security.device.audio-input"
codesign -dv --verbose=4 "$APP" 2>&1 | grep -E 'Identifier=|Signature=|TeamIdentifier=|flags=|Info.plist|Sealed Resources'
