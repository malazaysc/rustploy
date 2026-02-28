#!/usr/bin/env bash
set -euo pipefail

BASE_SHA="${1:-}"
HEAD_SHA="${2:-HEAD}"

if [[ -z "${BASE_SHA}" ]]; then
  echo "docs-guard: no base sha provided, skipping."
  exit 0
fi

if [[ "${BASE_SHA}" == "0000000000000000000000000000000000000000" ]]; then
  echo "docs-guard: zero base sha (new branch), skipping."
  exit 0
fi

if ! git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  echo "docs-guard: base sha not present locally, fetching origin history..."
  git fetch --no-tags --prune --depth=200 origin || true
fi

if ! git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  echo "docs-guard: unable to resolve base sha ${BASE_SHA}, skipping."
  exit 0
fi

changed_files="$(git diff --name-only "${BASE_SHA}" "${HEAD_SHA}")"
if [[ -z "${changed_files}" ]]; then
  echo "docs-guard: no changed files."
  exit 0
fi

if [[ "${DOCS_GUARD_BYPASS:-0}" == "1" ]]; then
  echo "docs-guard: bypassed by DOCS_GUARD_BYPASS=1"
  exit 0
fi

head_message="$(git log -1 --pretty=%B "${HEAD_SHA}" || true)"
if grep -q "\[skip-docs-guard\]" <<<"${head_message}"; then
  echo "docs-guard: bypassed by [skip-docs-guard] commit marker."
  exit 0
fi

matches_any() {
  local pattern="$1"
  grep -E -q "${pattern}" <<<"${changed_files}"
}

has_file() {
  local filename="$1"
  grep -F -x -q "${filename}" <<<"${changed_files}"
}

needs_docs=false
if matches_any '^(crates/|Dockerfile$|docker-compose\.yml$|openapi\.yaml$)'; then
  needs_docs=true
fi

if [[ "${needs_docs}" != "true" ]]; then
  echo "docs-guard: no behavior/runtime/api triggers found."
  exit 0
fi

errors=()

if ! has_file "docs/status.md"; then
  errors+=("docs/status.md must be updated when behavior/runtime/API changes.")
fi

if ! has_file "CHANGELOG.md"; then
  errors+=("CHANGELOG.md must be updated when behavior/runtime/API changes.")
fi

if matches_any '^(crates/server/src/lib\.rs|crates/shared/src/lib\.rs|crates/server/migrations/|openapi\.yaml$)'; then
  if ! has_file "openapi.yaml" && ! has_file "docs/api.md" && ! has_file "docs/api-contract.md"; then
    errors+=("API-related changes require at least one API doc update: openapi.yaml, docs/api.md, or docs/api-contract.md.")
  fi
fi

if [[ ${#errors[@]} -gt 0 ]]; then
  echo "docs-guard: FAILED"
  for err in "${errors[@]}"; do
    echo "- ${err}"
  done
  echo ""
  echo "Changed files:"
  echo "${changed_files}"
  exit 1
fi

echo "docs-guard: passed."
