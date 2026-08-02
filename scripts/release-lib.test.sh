#!/usr/bin/env bash
set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release-lib.sh
source "$script_dir/release-lib.sh"

failures=0

fail() {
  printf 'FAIL %s\n' "$1" >&2
  failures=$((failures + 1))
}

assert_compare() {
  local expected="$1"
  local left="$2"
  local right="$3"
  local actual
  if ! actual="$(kxen_compare_stable_release_tags "$left" "$right" 2>/dev/null)"; then
    fail "compare $left to $right returned an error"
  elif [[ "$actual" != "$expected" ]]; then
    fail "compare $left to $right expected $expected, got $actual"
  fi
}

assert_json_max() {
  local expected="$1"
  local requested="$2"
  local releases_json="$3"
  local actual
  if ! actual="$(printf '%s\n' "$releases_json" | kxen_latest_published_stable_tag_from_json "$requested" 2>/dev/null)"; then
    fail "JSON maximum for $requested returned an error"
  elif [[ "$actual" != "$expected" ]]; then
    fail "JSON maximum for $requested expected ${expected:-<empty>}, got ${actual:-<empty>}"
  fi
}

assert_json_rejected() {
  local label="$1"
  local requested="$2"
  local releases_json="$3"
  if printf '%s\n' "$releases_json" | kxen_latest_published_stable_tag_from_json "$requested" >/dev/null 2>&1; then
    fail "$label was accepted"
  fi
}

assert_compare 0 v1.2.3 v1.2.3
assert_compare 1 v2.0.0 v1.999.999
assert_compare 1 v1.10.0 v1.9.999
assert_compare 1 v1.0.1 v1.0.0
assert_compare -1 v0.9.9 v1.0.0
assert_compare 1 v184467440737095516160.0.0 v184467440737095516159.999.999
if kxen_compare_stable_release_tags v01.2.3 v1.2.3 >/dev/null 2>&1; then
  fail 'invalid comparison operand was accepted'
fi

releases='[
  [
    {"tag_name":"v1.2.3","draft":false,"prerelease":false},
    {"tag_name":"v99.0.0","draft":true,"prerelease":false},
    {"tag_name":"v100.0.0","draft":false,"prerelease":true}
  ],
  [
    {"tag_name":"v1.10.0","draft":false,"prerelease":false},
    {"tag_name":"v2.0.0","draft":false,"prerelease":false}
  ]
]'
assert_json_max v1.10.0 v2.0.0 "$releases"
assert_json_max '' v1.0.0 '[[
  {"tag_name":"v1.0.0","draft":false,"prerelease":false},
  {"tag_name":"v9.0.0","draft":true,"prerelease":false},
  {"tag_name":"v10.0.0","draft":false,"prerelease":true}
]]'
assert_json_rejected 'invalid published stable tag' v2.0.0 '[[{"tag_name":"v01.2.3","draft":false,"prerelease":false}]]'
assert_json_rejected 'malformed releases envelope' v2.0.0 '{"tag_name":"v1.2.3","draft":false,"prerelease":false}'
assert_json_rejected 'malformed release object' v2.0.0 '[[{"tag_name":"v1.2.3","prerelease":false}]]'

mock_gh_mode='success'
mock_releases="$releases"
gh() {
  if [[ "$mock_gh_mode" == failure ]]; then
    return 1
  fi
  printf '%s\n' "$mock_releases"
}
if ! kxen_require_release_above_published_stable v2.0.0 example/project >/dev/null 2>&1; then
  fail 'newer release was rejected by the API-backed gate'
fi
if kxen_require_release_above_published_stable v1.5.0 example/project >/dev/null 2>&1; then
  fail 'release below the published maximum was accepted'
fi
mock_releases='[[]]'
if ! kxen_require_release_above_published_stable v1.0.0 example/project >/dev/null 2>&1; then
  fail 'first stable release was rejected'
fi
mock_gh_mode='failure'
if kxen_require_release_above_published_stable v2.0.0 example/project >/dev/null 2>&1; then
  fail 'GitHub API failure was accepted'
fi

if [[ "$failures" -ne 0 ]]; then
  printf '%s release library test(s) failed\n' "$failures" >&2
  exit 1
fi
printf 'PASS release library SemVer and JSON tests\n'
