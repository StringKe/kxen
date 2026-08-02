#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release-lib.sh
source "$script_dir/release-lib.sh"

release_tag="${1:-}"
main_ref="${2:-origin/main}"
repository="${3:-StringKe/kxen}"

kxen_require_release_tag "$release_tag"
kxen_require_github_repository "$repository"
kxen_require_release_above_published_stable "$release_tag" "$repository"
version="${release_tag#v}"

if ! git show-ref --verify --quiet "refs/tags/$release_tag"; then
  printf 'release tag does not exist locally: %s\n' "$release_tag"
  exit 1
fi

tag_commit="$(git rev-parse --verify "$release_tag^{commit}")"
head_commit="$(git rev-parse --verify HEAD)"
main_commit="$(git rev-parse --verify "$main_ref^{commit}")"

if [[ "$head_commit" != "$tag_commit" ]]; then
  printf 'checked out commit %s does not match tag %s at %s\n' "$head_commit" "$release_tag" "$tag_commit"
  exit 1
fi
if ! git merge-base --is-ancestor "$tag_commit" "$main_commit"; then
  printf 'tag %s at %s is not an ancestor of %s at %s\n' "$release_tag" "$tag_commit" "$main_ref" "$main_commit"
  exit 1
fi

versions="$({
  KXEN_RELEASE_REPOSITORY="$repository" python3 - <<'PY'
import base64
import json
import os
import tomllib

def canonical_base64(value, label):
    try:
        decoded = base64.b64decode(value, validate=True)
    except Exception as error:
        raise SystemExit(f"{label} is not canonical base64: {error}") from error
    if base64.b64encode(decoded).decode() != value:
        raise SystemExit(f"{label} is not canonical base64")
    return decoded

with open("src-tauri/Cargo.toml", "rb") as handle:
    cargo_version = tomllib.load(handle)["package"]["version"]
with open("src-tauri/Cargo.lock", "rb") as handle:
    lock = tomllib.load(handle)
lock_versions = [package["version"] for package in lock["package"] if package["name"] == "kxen-app"]
if len(lock_versions) != 1:
    raise SystemExit(f"expected one kxen-app package in Cargo.lock, found {len(lock_versions)}")
with open("src-tauri/tauri.conf.json", encoding="utf-8") as handle:
    tauri = json.load(handle)
if tauri.get("bundle", {}).get("createUpdaterArtifacts") is not True:
    raise SystemExit("bundle.createUpdaterArtifacts must be true")
updater = tauri.get("plugins", {}).get("updater", {})
public_key = updater.get("pubkey")
if not isinstance(public_key, str) or not public_key:
    raise SystemExit("plugins.updater.pubkey must be configured")
try:
    public_key_box = canonical_base64(public_key, "plugins.updater.pubkey").decode().strip().splitlines()
except UnicodeDecodeError as error:
    raise SystemExit("plugins.updater.pubkey is not a UTF-8 minisign envelope") from error
if len(public_key_box) != 2 or not public_key_box[0].startswith("untrusted comment: "):
    raise SystemExit("plugins.updater.pubkey is not a minisign public key envelope")
public_key_packet = canonical_base64(public_key_box[1], "minisign public key")
if len(public_key_packet) != 42 or public_key_packet[:2] != b"Ed":
    raise SystemExit("plugins.updater.pubkey contains an unsupported minisign public key")
endpoints = updater.get("endpoints", [])
expected_endpoint = f"https://github.com/{os.environ['KXEN_RELEASE_REPOSITORY']}/releases/latest/download/latest.json"
if endpoints != [expected_endpoint]:
    raise SystemExit(f"plugins.updater.endpoints must equal [{expected_endpoint!r}]")
print(cargo_version, tauri["version"], lock_versions[0])
PY
} 2>&1)" || {
  printf '%s\n' "$versions"
  exit 1
}
IFS=' ' read -r cargo_version tauri_version lock_version <<< "$versions"

for pair in "Cargo.toml:$cargo_version" "Cargo.lock:$lock_version" "tauri.conf.json:$tauri_version"; do
  source_name="${pair%%:*}"
  source_version="${pair#*:}"
  if [[ "$source_version" != "$version" ]]; then
    printf '%s version %s does not match release tag %s\n' "$source_name" "$source_version" "$release_tag"
    exit 1
  fi
done

release_heading="## [$version]"
heading_count="$(grep -Fxc "$release_heading" CHANGELOG.md || true)"
if [[ "$heading_count" != 1 ]]; then
  printf 'CHANGELOG.md must contain exactly one release heading: %s\n' "$release_heading"
  exit 1
fi
if ! awk -v heading="$release_heading" '
  $0 == heading { capture = 1; next }
  capture && /^## / { exit }
  capture && /[^[:space:]]/ { found = 1 }
  END { exit found != 1 }
' CHANGELOG.md; then
  printf 'CHANGELOG.md release section is empty: %s\n' "$release_heading"
  exit 1
fi

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    printf 'tag=%s\n' "$release_tag"
    printf 'version=%s\n' "$version"
    printf 'commit=%s\n' "$tag_commit"
  } >> "$GITHUB_OUTPUT"
fi

printf 'PASS release tag: %s\n' "$release_tag"
printf 'PASS release commit: %s is an ancestor of %s\n' "$tag_commit" "$main_ref"
printf 'PASS release version: %s\n' "$version"
printf 'PASS updater configuration\n'
