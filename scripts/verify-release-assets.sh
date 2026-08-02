#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release-lib.sh
source "$script_dir/release-lib.sh"

release_tag="${1:-}"
repository="${2:-}"
asset_dir="${3:-release-assets}"

kxen_require_release_tag "$release_tag"
kxen_require_github_repository "$repository"
if [[ ! -d "$asset_dir" ]]; then
  printf 'release asset directory not found: %s\n' "$asset_dir"
  exit 1
fi

dmg_path="$(kxen_find_one DMG "$asset_dir" 'Kxen_*_aarch64.dmg')"
updater_path="$(kxen_find_one 'updater archive' "$asset_dir" '*.app.tar.gz')"
signature_path="$(kxen_find_one 'updater signature' "$asset_dir" '*.app.tar.gz.sig')"
latest_path="$asset_dir/latest.json"
checksums_path="$asset_dir/SHA256SUMS"

kxen_require_regular_file_size DMG "$dmg_path" 2147483648
kxen_require_regular_file_size 'updater archive' "$updater_path" 536870912
kxen_require_regular_file_size 'updater signature' "$signature_path" 65536
kxen_require_regular_file_size 'updater manifest' "$latest_path" 1048576
kxen_require_regular_file_size 'checksum manifest' "$checksums_path" 65536

entry_count="$(find "$asset_dir" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')"
file_count="$(find "$asset_dir" -mindepth 1 -maxdepth 1 -type f -print | wc -l | tr -d ' ')"
if [[ "$entry_count" != 5 || "$file_count" != 5 ]]; then
  printf 'release asset directory must contain exactly five regular files: entries=%s files=%s\n' "$entry_count" "$file_count"
  exit 1
fi

dmg_name="$(basename "$dmg_path")"
updater_name="$(basename "$updater_path")"
signature_name="$(basename "$signature_path")"
version="${release_tag#v}"
if [[ "$dmg_name" != "Kxen_${version}_aarch64.dmg" ]]; then
  printf 'DMG name does not match release version: %s\n' "$dmg_name"
  exit 1
fi
if [[ "$updater_name" != 'Kxen.app.tar.gz' || "$signature_name" != "$updater_name.sig" ]]; then
  printf 'updater archive and signature names do not match Kxen.app: %s %s\n' "$updater_name" "$signature_name"
  exit 1
fi
for name in "$dmg_name" "$updater_name" "$signature_name" latest.json; do
  if ! awk -v expected="$name" '$2 == expected { found += 1 } END { exit found != 1 }' "$checksums_path"; then
    printf 'checksum manifest must contain exactly one entry for %s\n' "$name"
    exit 1
  fi
done
if [[ "$(wc -l < "$checksums_path" | tr -d ' ')" != 4 ]]; then
  printf 'checksum manifest must contain exactly four entries\n'
  exit 1
fi
(
  cd "$asset_dir"
  shasum -a 256 -c SHA256SUMS
)

app_identifier="$(jq -er '.identifier | select(type == "string" and length > 0)' src-tauri/tauri.conf.json)"
kxen_verify_macos_updater_archive "$updater_path" "$version" "$app_identifier"
kxen_verify_updater_signature "$updater_path" "$signature_path"
signature="$(cat "$signature_path")"
updater_url="https://github.com/$repository/releases/download/$release_tag/$updater_name"

jq -e \
  --arg version "$version" \
  --arg signature "$signature" \
  --arg url "$updater_url" \
  '
    ((keys | sort) == ["notes", "platforms", "pub_date", "version"]) and
    .version == $version and
    (.notes | type == "string" and length > 0) and
    (.pub_date | type == "string" and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")) and
    ((.platforms | keys | sort) == ["darwin-aarch64", "darwin-aarch64-app"]) and
    ((.platforms["darwin-aarch64"] | keys | sort) == ["signature", "url"]) and
    ((.platforms["darwin-aarch64-app"] | keys | sort) == ["signature", "url"]) and
    .platforms["darwin-aarch64"].signature == $signature and
    .platforms["darwin-aarch64"].url == $url and
    .platforms["darwin-aarch64-app"].signature == $signature and
    .platforms["darwin-aarch64-app"].url == $url
  ' "$latest_path" >/dev/null

printf 'PASS release checksums\n'
printf 'PASS updater archive: %s\n' "$updater_name"
printf 'PASS updater manifest: %s -> %s\n' "$version" "$updater_url"
