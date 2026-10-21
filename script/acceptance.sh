#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "== TypeScript build =="
npm run build

echo "== Rust format =="
cargo fmt --all --check

echo "== Rust tests =="
cargo test --workspace

echo "== Tauri macOS app bundle =="
npm run tauri -- build --debug --bundles app
./script/sign_macos_app.sh debug

echo "== Bundle metadata =="
plutil -p target/debug/bundle/macos/OrbShare.app/Contents/Info.plist \
  | rg 'CFBundleIcon|CFBundleIdentifier|CFBundleName|LSMinimumSystemVersion'

minimum_macos_version="$(plutil -extract LSMinimumSystemVersion raw \
  target/debug/bundle/macos/OrbShare.app/Contents/Info.plist)"
if [[ "$minimum_macos_version" != "12.3" ]]; then
  echo "Expected LSMinimumSystemVersion=12.3, got $minimum_macos_version" >&2
  exit 1
fi

echo "== Bundle signatures =="
codesign --verify --deep --strict --verbose=2 target/debug/bundle/macos/OrbShare.app

if ! codesign -d --verbose=4 target/debug/bundle/macos/OrbShare.app 2>&1 \
  | rg -q 'flags=.*runtime'; then
  echo "OrbShare.app is missing the Hardened Runtime code-signing flag" >&2
  exit 1
fi

if ! codesign -d -r- target/debug/bundle/macos/OrbShare.app 2>&1 \
  | rg -q 'designated => identifier "com\.orbshare\.desktop"'; then
  echo "OrbShare.app is missing the stable local designated requirement" >&2
  exit 1
fi

echo "== Acceptance baseline complete =="
