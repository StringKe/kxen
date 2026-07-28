#!/usr/bin/env bash
set -euo pipefail

bundle_root="${1:-src-tauri/target/aarch64-apple-darwin/release/bundle}"
app_path="$bundle_root/macos/Kxen.app"
dmg_path="$(
  find "$bundle_root/dmg" -maxdepth 1 -type f -name 'Kxen_*_aarch64.dmg' -print -quit
)"

if [[ ! -d "$app_path" ]]; then
  printf 'signed app not found: %s\n' "$app_path"
  exit 1
fi
if [[ -z "$dmg_path" || ! -s "$dmg_path" ]]; then
  printf 'signed DMG not found below: %s\n' "$bundle_root/dmg"
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$app_path"
spctl --assess --type execute --verbose=2 "$app_path"
xcrun stapler validate "$app_path"

codesign --verify --strict --verbose=2 "$dmg_path"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg_path"

printf 'PASS signed app: %s\n' "$app_path"
printf 'PASS signed Gatekeeper-accepted DMG containing the notarized app: %s\n' "$dmg_path"
