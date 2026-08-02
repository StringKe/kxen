#!/usr/bin/env bash

kxen_require_release_tag() {
  local release_tag="$1"
  local pattern='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
  if [[ ! "$release_tag" =~ $pattern ]]; then
    printf 'invalid stable release tag: %s\n' "$release_tag"
    return 1
  fi
}

kxen_require_github_repository() {
  local repository="$1"
  if [[ ! "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
    printf 'invalid GitHub repository: %s\n' "$repository"
    return 1
  fi
}

kxen_compare_decimal_release_components() {
  local left="$1"
  local right="$2"
  local LC_ALL=C
  if [[ "${#left}" -gt "${#right}" ]]; then
    printf '1\n'
  elif [[ "${#left}" -lt "${#right}" ]]; then
    printf '%s\n' '-1'
  elif [[ "$left" == "$right" ]]; then
    printf '0\n'
  elif [[ "$left" > "$right" ]]; then
    printf '1\n'
  else
    printf '%s\n' '-1'
  fi
}

kxen_compare_stable_release_tags() {
  local left="$1"
  local right="$2"
  local left_major left_minor left_patch
  local right_major right_minor right_patch
  local comparison
  kxen_require_release_tag "$left" >/dev/null || return 1
  kxen_require_release_tag "$right" >/dev/null || return 1
  IFS=. read -r left_major left_minor left_patch <<< "${left#v}"
  IFS=. read -r right_major right_minor right_patch <<< "${right#v}"
  for comparison in \
    "$(kxen_compare_decimal_release_components "$left_major" "$right_major")" \
    "$(kxen_compare_decimal_release_components "$left_minor" "$right_minor")" \
    "$(kxen_compare_decimal_release_components "$left_patch" "$right_patch")"; do
    if [[ "$comparison" != 0 ]]; then
      printf '%s\n' "$comparison"
      return 0
    fi
  done
  printf '0\n'
}

kxen_latest_published_stable_tag_from_json() {
  local requested_tag="$1"
  local stable_pattern='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'
  local tags
  local tag
  local latest=''
  local comparison
  kxen_require_release_tag "$requested_tag" >/dev/null || return 1
  tags="$({
    jq -r \
      --arg requested "$requested_tag" \
      --arg stable_pattern "$stable_pattern" \
      '
        if type != "array" or any(.[]; type != "array") then
          error("GitHub releases response must be an array of page arrays")
        else
          [
            .[][] |
            if type != "object"
              or (.tag_name | type) != "string"
              or (.draft | type) != "boolean"
              or (.prerelease | type) != "boolean"
            then
              error("GitHub release entry has invalid tag_name, draft, or prerelease fields")
            elif .draft == false and .prerelease == false and .tag_name != $requested then
              if (.tag_name | test($stable_pattern)) then
                .tag_name
              else
                error("invalid published stable release tag: \(.tag_name)")
              end
            else
              empty
            end
          ] | .[]
        end
      '
  } 2>&1)" || {
    printf '%s\n' "$tags" >&2
    return 1
  }
  while IFS= read -r tag; do
    [[ -n "$tag" ]] || continue
    if [[ -z "$latest" ]]; then
      latest="$tag"
      continue
    fi
    comparison="$(kxen_compare_stable_release_tags "$tag" "$latest")" || return 1
    if [[ "$comparison" == 1 ]]; then
      latest="$tag"
    fi
  done <<< "$tags"
  if [[ -n "$latest" ]]; then
    printf '%s\n' "$latest"
  fi
}

kxen_require_release_above_published_stable() {
  local release_tag="$1"
  local repository="$2"
  local pages
  local latest
  local comparison
  kxen_require_release_tag "$release_tag" || return 1
  kxen_require_github_repository "$repository" || return 1
  if ! pages="$(gh api --paginate "repos/$repository/releases?per_page=100" --slurp)"; then
    printf 'unable to list published releases for %s\n' "$repository" >&2
    return 1
  fi
  if ! latest="$(printf '%s\n' "$pages" | kxen_latest_published_stable_tag_from_json "$release_tag")"; then
    printf 'unable to determine the published stable release baseline for %s\n' "$repository" >&2
    return 1
  fi
  if [[ -z "$latest" ]]; then
    printf 'PASS no prior published stable release exists for %s\n' "$repository"
    return 0
  fi
  comparison="$(kxen_compare_stable_release_tags "$release_tag" "$latest")" || return 1
  if [[ "$comparison" != 1 ]]; then
    printf 'release tag %s must be strictly newer than published stable release %s\n' "$release_tag" "$latest" >&2
    return 1
  fi
  printf 'PASS release tag %s is newer than published stable release %s\n' "$release_tag" "$latest"
}

kxen_find_one() {
  local label="$1"
  local directory="$2"
  local pattern="$3"
  local matches=()
  while IFS= read -r path; do
    matches+=("$path")
  done < <(find "$directory" -maxdepth 1 -type f -name "$pattern" -print | sort)
  if [[ "${#matches[@]}" -ne 1 ]]; then
    printf 'expected exactly one %s below %s, found %s\n' "$label" "$directory" "${#matches[@]}" >&2
    return 1
  fi
  printf '%s\n' "${matches[0]}"
}

kxen_require_regular_file_size() {
  local label="$1"
  local path="$2"
  local maximum_bytes="$3"
  python3 - "$label" "$path" "$maximum_bytes" <<'PY'
import os
import stat
import sys

label, path, maximum_bytes_raw = sys.argv[1:]
maximum_bytes = int(maximum_bytes_raw)
try:
    metadata = os.lstat(path)
except OSError as error:
    raise SystemExit(f"{label} cannot be inspected: {path}: {error}") from error
if not stat.S_ISREG(metadata.st_mode):
    raise SystemExit(f"{label} must be a regular file: {path}")
if metadata.st_size <= 0:
    raise SystemExit(f"{label} is empty: {path}")
if metadata.st_size > maximum_bytes:
    raise SystemExit(f"{label} exceeds {maximum_bytes} bytes: {path}")
PY
}

kxen_verify_macos_updater_archive() {
  local archive_path="$1"
  local expected_version="${2:-}"
  local expected_identifier="${3:-}"
  python3 - "$archive_path" "$expected_version" "$expected_identifier" <<'PY'
import posixpath
from pathlib import PurePosixPath
import plistlib
import os
import stat
import sys
import tarfile

archive_path = sys.argv[1]
expected_version = sys.argv[2]
expected_identifier = sys.argv[3]
archive_stat = os.lstat(archive_path)
if not stat.S_ISREG(archive_stat.st_mode):
    raise SystemExit("updater archive must be a regular file")
if archive_stat.st_size > 512 * 1024 * 1024:
    raise SystemExit("updater archive exceeds 512 MiB")
info_plist_data = None
has_executable = False
seen_names = set()
member_count = 0
total_size = 0
with tarfile.open(archive_path, mode="r:gz") as archive:
    for member in archive:
        member_count += 1
        if member_count > 100_000:
            raise SystemExit("updater archive contains more than 100000 entries")
        if member.size < 0:
            raise SystemExit(f"updater archive entry has a negative size: {member.name}")
        total_size += member.size
        if total_size > 2 * 1024 * 1024 * 1024:
            raise SystemExit("updater archive expands beyond 2 GiB")
        name = member.name
        canonical_name = PurePosixPath(name).as_posix()
        comparable_name = name.rstrip("/") if member.isdir() else name
        if "\\" in name or comparable_name != canonical_name:
            raise SystemExit(f"non-canonical updater archive entry: {name}")
        if canonical_name in seen_names:
            raise SystemExit(f"duplicate updater archive entry: {name}")
        seen_names.add(canonical_name)
        parts = PurePosixPath(name).parts
        if name.startswith("/") or ".." in parts or not parts or parts[0] != "Kxen.app":
            raise SystemExit(f"unsafe updater archive entry: {name}")
        if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
            raise SystemExit(f"unsupported updater archive entry type: {name}")
        if member.issym() or member.islnk():
            if canonical_name in {
                "Kxen.app",
                "Kxen.app/Contents",
                "Kxen.app/Contents/MacOS",
            }:
                raise SystemExit(f"critical updater archive directory cannot be a link: {name}")
            target = member.linkname
            if target.startswith("/") or "\\" in target:
                raise SystemExit(f"unsafe updater archive link: {name} -> {target}")
            if member.issym():
                target = posixpath.join(posixpath.dirname(name), target)
            resolved = posixpath.normpath(target)
            if resolved != "Kxen.app" and not resolved.startswith("Kxen.app/"):
                raise SystemExit(f"unsafe updater archive link: {name} -> {member.linkname}")
        if member.isfile() and name == "Kxen.app/Contents/Info.plist":
            info_file = archive.extractfile(member)
            if info_file is None:
                raise SystemExit("unable to read updater Info.plist")
            info_plist_data = info_file.read(1_048_577)
            if len(info_plist_data) > 1_048_576:
                raise SystemExit("updater Info.plist exceeds 1 MiB")
        if (
            member.isfile()
            and name.startswith("Kxen.app/Contents/MacOS/")
            and member.mode & 0o111
        ):
            has_executable = True
if member_count == 0:
    raise SystemExit("updater archive is empty")
if info_plist_data is None:
    raise SystemExit("updater archive does not contain Kxen.app/Contents/Info.plist")
if not has_executable:
    raise SystemExit("updater archive does not contain an executable under Kxen.app/Contents/MacOS")
try:
    info = plistlib.loads(info_plist_data)
except Exception as error:
    raise SystemExit(f"updater Info.plist is invalid: {error}") from error
if info.get("CFBundlePackageType") != "APPL":
    raise SystemExit("updater Info.plist is not an application bundle")
if expected_version and info.get("CFBundleShortVersionString") != expected_version:
    raise SystemExit("updater Info.plist version does not match the release")
if expected_identifier and info.get("CFBundleIdentifier") != expected_identifier:
    raise SystemExit("updater Info.plist identifier does not match the application")
PY
}

kxen_verify_updater_signature() {
  local archive_path="$1"
  local signature_path="$2"
  local tauri_config="${3:-src-tauri/tauri.conf.json}"
  local public_key
  public_key="$(jq -er '.plugins.updater.pubkey | select(type == "string" and length > 0)' "$tauri_config")"
  local library_dir
  library_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  node "$library_dir/verify-updater-signature.mjs" "$archive_path" "$signature_path" "$public_key"
}
