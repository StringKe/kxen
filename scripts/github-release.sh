#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release-lib.sh
source "$script_dir/release-lib.sh"

operation="${1:-}"
release_tag="${2:-}"
repository="${3:-}"
release_commit="${4:-}"
asset_dir="${5:-release-assets}"

kxen_require_release_tag "$release_tag"
kxen_require_github_repository "$repository"
if [[ ! "$release_commit" =~ ^[0-9a-f]{40}$ ]]; then
  printf 'invalid release commit: %s\n' "$release_commit"
  exit 1
fi
if [[ ! "${GITHUB_RUN_ID:-}" =~ ^[0-9]+$ || ! "${GITHUB_RUN_ATTEMPT:-}" =~ ^[0-9]+$ ]]; then
  printf 'GITHUB_RUN_ID and GITHUB_RUN_ATTEMPT must be numeric\n'
  exit 1
fi

draft_marker="<!-- kxen-release-workflow:${GITHUB_RUN_ID}:${GITHUB_RUN_ATTEMPT} -->"
owned_marker_prefix='<!-- kxen-release-workflow:'

find_release() {
  local pages
  local count
  pages="$(gh api --paginate "repos/$repository/releases?per_page=100" --slurp)"
  count="$(jq -r --arg tag "$release_tag" '[.[][] | select(.tag_name == $tag)] | length' <<< "$pages")"
  if [[ "$count" -gt 1 ]]; then
    printf 'GitHub returned multiple releases for tag: %s\n' "$release_tag" >&2
    return 1
  fi
  if [[ "$count" == 1 ]]; then
    jq -c --arg tag "$release_tag" '.[][] | select(.tag_name == $tag)' <<< "$pages"
  fi
}

assert_remote_source() {
  local object
  local object_type
  local object_sha
  local main_sha
  local relationship
  object="$(
    gh api "repos/$repository/git/ref/tags/$release_tag" \
      --jq '.object | [.type, .sha] | @tsv'
  )"
  IFS=$'\t' read -r object_type object_sha <<< "$object"
  for _ in 1 2 3 4 5 6 7 8; do
    if [[ "$object_type" != tag ]]; then
      break
    fi
    object="$(
      gh api "repos/$repository/git/tags/$object_sha" \
        --jq '.object | [.type, .sha] | @tsv'
    )"
    IFS=$'\t' read -r object_type object_sha <<< "$object"
  done
  if [[ "$object_type" != commit || "$object_sha" != "$release_commit" ]]; then
    printf 'remote tag does not resolve to the validated commit: %s %s\n' \
      "$object_type" "$object_sha"
    return 1
  fi
  main_sha="$(gh api "repos/$repository/git/ref/heads/main" --jq '.object.sha')"
  relationship="$(
    gh api "repos/$repository/compare/$release_commit...$main_sha" --jq '.status'
  )"
  if [[ "$relationship" != ahead && "$relationship" != identical ]]; then
    printf 'validated commit is not an ancestor of remote main: %s\n' "$relationship"
    return 1
  fi
}

require_current_draft() {
  local release_json="$1"
  jq -e \
    --arg tag "$release_tag" \
    --arg marker "$draft_marker" \
    '.tag_name == $tag and .draft == true and ((.body // "") | contains($marker))' \
    <<< "$release_json" >/dev/null
}

create_draft() {
  local existing
  local release_notes
  assert_remote_source
  kxen_require_release_above_published_stable "$release_tag" "$repository"
  existing="$(find_release)"
  if [[ -n "$existing" ]]; then
    if ! jq -e '.draft == true' <<< "$existing" >/dev/null; then
      printf 'published release already exists; refusing to overwrite it: %s\n' "$release_tag"
      return 1
    fi
    if ! jq -e --arg prefix "$owned_marker_prefix" \
      '((.body // "") | contains($prefix))' <<< "$existing" >/dev/null; then
      printf 'existing draft is not owned by this workflow; refusing to delete it: %s\n' "$release_tag"
      return 1
    fi
    gh release delete "$release_tag" --repo "$repository" --yes
    printf 'removed incomplete workflow-owned draft: %s\n' "$release_tag"
  fi
  release_notes="$(
    printf 'macOS 14+ Apple Silicon signed and notarized build.\n\n%s\n' "$draft_marker"
  )"
  gh release create "$release_tag" \
    --repo "$repository" \
    --verify-tag \
    --target "$release_commit" \
    --draft \
    --title "Kxen $release_tag development preview" \
    --notes "$release_notes" \
    "$asset_dir"/*
  require_current_draft "$(find_release)"
}

verify_draft() {
  local release_json
  local expected
  local actual
  local remote_dir
  assert_remote_source
  release_json="$(find_release)"
  require_current_draft "$release_json"
  jq -e '[.assets[] | select(.state != "uploaded" or .size <= 0)] | length == 0' \
    <<< "$release_json" >/dev/null
  expected="$(find "$asset_dir" -maxdepth 1 -type f -exec basename {} \; | sort)"
  actual="$(jq -r '.assets[].name' <<< "$release_json" | sort)"
  if [[ "$actual" != "$expected" ]]; then
    printf 'draft release assets do not match the verified set\nexpected:\n%s\nactual:\n%s\n' \
      "$expected" "$actual"
    return 1
  fi
  remote_dir="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/kxen-remote-assets.XXXXXX")"
  gh release download "$release_tag" --repo "$repository" --dir "$remote_dir"
  bash "$script_dir/verify-release-assets.sh" "$release_tag" "$repository" "$remote_dir"
  while IFS= read -r local_path; do
    cmp "$local_path" "$remote_dir/$(basename "$local_path")"
  done < <(find "$asset_dir" -maxdepth 1 -type f -print | sort)
}

publish_release() {
  local release_json
  local latest_tag
  assert_remote_source
  release_json="$(find_release)"
  require_current_draft "$release_json"
  kxen_require_release_above_published_stable "$release_tag" "$repository"
  gh release edit "$release_tag" \
    --repo "$repository" \
    --draft=false \
    --prerelease=false \
    --latest
  assert_remote_source
  release_json="$(find_release)"
  jq -e \
    --arg tag "$release_tag" \
    --arg marker "$draft_marker" \
    '.tag_name == $tag and .draft == false and ((.body // "") | contains($marker))' \
    <<< "$release_json" >/dev/null
  latest_tag="$(gh api "repos/$repository/releases/latest" --jq '.tag_name')"
  if [[ "$latest_tag" != "$release_tag" ]]; then
    printf 'published release is not the repository latest release: %s\n' "$latest_tag"
    return 1
  fi
  jq -r '.html_url' <<< "$release_json"
}

cleanup_draft() {
  local release_json
  release_json="$(find_release)"
  if [[ -z "$release_json" ]]; then
    return 0
  fi
  if jq -e --arg marker "$draft_marker" \
    '.draft == true and ((.body // "") | contains($marker))' \
    <<< "$release_json" >/dev/null; then
    gh release delete "$release_tag" --repo "$repository" --yes
    printf 'removed incomplete draft created by this run: %s\n' "$release_tag"
  else
    printf 'release was not a draft owned by this run; cleanup left it unchanged: %s\n' \
      "$release_tag"
  fi
}

case "$operation" in
  create-draft) create_draft ;;
  verify-draft) verify_draft ;;
  publish) publish_release ;;
  cleanup-draft) cleanup_draft ;;
  *)
    printf 'usage: github-release.sh <create-draft|verify-draft|publish|cleanup-draft> <tag> <repository> <commit> [asset-dir]\n'
    exit 1
    ;;
esac
