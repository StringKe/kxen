#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release-lib.sh
source "$script_dir/release-lib.sh"

release_tag="${1:-}"
repository="${2:-}"
bundle_root="${3:-src-tauri/target/aarch64-apple-darwin/release/bundle}"
output_dir="${4:-release-assets}"

kxen_require_release_tag "$release_tag"
kxen_require_github_repository "$repository"

dmg_path="$(kxen_find_one DMG "$bundle_root/dmg" 'Kxen_*_aarch64.dmg')"
updater_path="$(kxen_find_one 'updater archive' "$bundle_root/macos" '*.app.tar.gz')"
signature_path="$(kxen_find_one 'updater signature' "$bundle_root/macos" '*.app.tar.gz.sig')"
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

for name in "$dmg_name" "$updater_name" "$signature_name"; do
  if [[ ! "$name" =~ ^[A-Za-z0-9._+-]+$ ]]; then
    printf 'release asset name requires URL encoding and is refused: %s\n' "$name"
    exit 1
  fi
done

kxen_require_regular_file_size DMG "$dmg_path" 2147483648
kxen_require_regular_file_size 'updater archive' "$updater_path" 536870912
kxen_require_regular_file_size 'updater signature' "$signature_path" 65536
app_identifier="$(jq -er '.identifier | select(type == "string" and length > 0)' src-tauri/tauri.conf.json)"
kxen_verify_macos_updater_archive "$updater_path" "$version" "$app_identifier"
signature="$(cat "$signature_path")"
if [[ -z "$signature" ]]; then
  printf 'updater signature is empty: %s\n' "$signature_name"
  exit 1
fi
kxen_verify_updater_signature "$updater_path" "$signature_path"

mkdir -p "$output_dir"
if [[ -n "$(find "$output_dir" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
  printf 'release asset directory must be empty: %s\n' "$output_dir"
  exit 1
fi
cp -p "$dmg_path" "$updater_path" "$signature_path" "$output_dir/"

updater_url="https://github.com/$repository/releases/download/$release_tag/$updater_name"
pub_date="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
jq -n \
  --arg version "$version" \
  --arg notes "Kxen $release_tag development preview." \
  --arg pub_date "$pub_date" \
  --arg signature "$signature" \
  --arg url "$updater_url" \
  '{
    version: $version,
    notes: $notes,
    pub_date: $pub_date,
    platforms: {
      "darwin-aarch64": { signature: $signature, url: $url },
      "darwin-aarch64-app": { signature: $signature, url: $url }
    }
  }' > "$output_dir/latest.json"

(
  cd "$output_dir"
  shasum -a 256 "$dmg_name" "$updater_name" "$signature_name" latest.json > SHA256SUMS
)

bash "$script_dir/verify-release-assets.sh" "$release_tag" "$repository" "$output_dir"
printf 'PASS prepared release assets: %s\n' "$output_dir"
