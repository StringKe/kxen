#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release-lib.sh
source "$script_dir/release-lib.sh"

bundle_root="${1:-src-tauri/target/aarch64-apple-darwin/release/bundle}"
app_path="$bundle_root/macos/Kxen.app"

dmg_path="$(kxen_find_one DMG "$bundle_root/dmg" 'Kxen_*_aarch64.dmg')"
updater_path="$(kxen_find_one 'updater archive' "$bundle_root/macos" '*.app.tar.gz')"
signature_path="$updater_path.sig"

if [[ ! -d "$app_path" || -L "$app_path" ]]; then
  printf 'signed app not found: %s\n' "$app_path"
  exit 1
fi
kxen_require_regular_file_size DMG "$dmg_path" 2147483648
kxen_require_regular_file_size 'updater archive' "$updater_path" 536870912
kxen_require_regular_file_size 'updater signature' "$signature_path" 65536

app_version="$(jq -er '.version | select(type == "string" and length > 0)' src-tauri/tauri.conf.json)"
app_identifier="$(jq -er '.identifier | select(type == "string" and length > 0)' src-tauri/tauri.conf.json)"
kxen_verify_macos_updater_archive "$updater_path" "$app_version" "$app_identifier"
kxen_verify_updater_signature "$updater_path" "$signature_path"

verify_bundle_metadata() {
  local bundle_path="$1"
  local info_path="$bundle_path/Contents/Info.plist"
  local version
  local identifier
  version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$info_path")"
  identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$info_path")"
  if [[ "$version" != "$app_version" || "$identifier" != "$app_identifier" ]]; then
    printf 'application bundle metadata does not match Tauri config: %s\n' "$bundle_path"
    return 1
  fi
}

codesign_cdhash() {
  codesign -d --verbose=4 "$1" 2>&1 | awk -F= '/^CDHash=/ { print $2; exit }'
}

verify_bundle_metadata "$app_path"
codesign --verify --deep --strict --verbose=2 "$app_path"
spctl --assess --type execute --verbose=2 "$app_path"
xcrun stapler validate "$app_path"
local_cdhash="$(codesign_cdhash "$app_path")"
if [[ -z "$local_cdhash" ]]; then
  printf 'verified application does not expose a CDHash\n'
  exit 1
fi

temp_root="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
if [[ ! -d "$temp_root" ]]; then
  printf 'temporary directory root does not exist: %s\n' "$temp_root"
  exit 1
fi
temp_root="$(cd "$temp_root" && pwd -P)"
updater_extract_path="$(mktemp -d "$temp_root/kxen-updater.XXXXXX")"
cleanup_updater_extract() {
  local expected_prefix="$temp_root/kxen-updater."
  local suffix="${updater_extract_path#"$expected_prefix"}"
  if [[ -z "$updater_extract_path" || "$updater_extract_path" != "$expected_prefix"* || ! "$suffix" =~ ^[[:alnum:]]{6}$ ]]; then
    printf 'refusing to recursively clean unexpected updater temp path: %s\n' "$updater_extract_path" >&2
    return 1
  fi
  if [[ -L "$updater_extract_path" ]]; then
    printf 'refusing to recursively clean a symlinked updater temp path: %s\n' "$updater_extract_path" >&2
    return 1
  fi
  if [[ -d "$updater_extract_path" ]]; then
    rm -rf -- "$updater_extract_path"
  fi
}
trap cleanup_updater_extract EXIT
tar -xzf "$updater_path" -C "$updater_extract_path"
updater_app_path="$updater_extract_path/Kxen.app"
if [[ ! -d "$updater_app_path" ]]; then
  printf 'updater archive did not extract one Kxen.app\n'
  exit 1
fi
verify_bundle_metadata "$updater_app_path"
codesign --verify --deep --strict --verbose=2 "$updater_app_path"
spctl --assess --type execute --verbose=2 "$updater_app_path"
xcrun stapler validate "$updater_app_path"
updater_cdhash="$(codesign_cdhash "$updater_app_path")"
if [[ "$updater_cdhash" != "$local_cdhash" ]]; then
  printf 'updater application CDHash does not match the verified build\n'
  exit 1
fi
cleanup_updater_extract
trap - EXIT

codesign --verify --strict --verbose=2 "$dmg_path"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg_path"

dmg_mount_path="$(mktemp -d "$temp_root/kxen-dmg.XXXXXX")"
dmg_mounted=0
cleanup_dmg_mount() {
  if [[ "$dmg_mounted" == 1 ]]; then
    hdiutil detach "$dmg_mount_path" >/dev/null 2>&1 || true
  fi
  rmdir "$dmg_mount_path" 2>/dev/null || true
}
trap cleanup_dmg_mount EXIT
hdiutil attach -readonly -nobrowse -mountpoint "$dmg_mount_path" "$dmg_path" >/dev/null
dmg_mounted=1
dmg_app_count="$(find "$dmg_mount_path" -maxdepth 1 -type d -name '*.app' -print | wc -l | tr -d ' ')"
dmg_app_path="$dmg_mount_path/Kxen.app"
if [[ "$dmg_app_count" != 1 || ! -d "$dmg_app_path" ]]; then
  printf 'DMG must contain exactly one top-level Kxen.app\n'
  exit 1
fi
verify_bundle_metadata "$dmg_app_path"
codesign --verify --deep --strict --verbose=2 "$dmg_app_path"
spctl --assess --type execute --verbose=2 "$dmg_app_path"
xcrun stapler validate "$dmg_app_path"
dmg_cdhash="$(codesign_cdhash "$dmg_app_path")"
if [[ "$dmg_cdhash" != "$local_cdhash" ]]; then
  printf 'DMG application CDHash does not match the verified build\n'
  exit 1
fi
hdiutil detach "$dmg_mount_path" >/dev/null
dmg_mounted=0
rmdir "$dmg_mount_path"
trap - EXIT

printf 'PASS signed app: %s\n' "$app_path"
printf 'PASS signed Gatekeeper-accepted DMG containing the notarized app: %s\n' "$dmg_path"
printf 'PASS updater archive and signature: %s\n' "$updater_path"
