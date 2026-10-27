#!/usr/bin/env bash
# Prints "code=true|false" and "examples=true|false": whether a change touches source code or a
# build/test input, and whether it touches a Docker Compose example.
# Pull requests are judged by their file list against an allowlist (see is_code); anything not
# listed (docs, specs, README, CHANGELOG, LICENSE, .claude/*, .github/*, scripts/ci/*, ...) skips
# the heavy jobs. The compose examples in examples/<name>/ are no Rust or image input, so they
# only set examples=true, which runs the examples job alone; examples/*.md is plain docs.
# examples/*.rs and examples/bot.example.toml stay code (a cargo example and a config the tests
# load). Every other trigger (push to main, manual run) counts as a code change so main is always
# fully tested. Because workflow and CI-script changes alone skip the jobs, they are only fully
# exercised on the push to main or via workflow_dispatch. Needs GH_TOKEN, GITHUB_EVENT_NAME,
# GITHUB_REPOSITORY and, for pull requests, PR_NUMBER.
set -euo pipefail

if [[ "$GITHUB_EVENT_NAME" != "pull_request" ]]; then
  echo "code=true"
  echo "examples=true"
  exit 0
fi

# Allowlist: only these paths can change what gets built or tested.
is_code() {
  case "$1" in
    scripts/ci/* | examples/*/* | examples/*.md) return 1 ;;
    # Rust
    *.rs | Cargo.toml | Cargo.lock | .cargo/* | rust-toolchain* | clippy.toml | rustfmt.toml \
      | src/* | tests/* | examples/* | assets/*) return 0 ;;
    # Image, test server and other build inputs
    Dockerfile | .dockerignore | testenv/* | scripts/*) return 0 ;;
    *) return 1 ;;
  esac
}

code=false
examples=false
while IFS= read -r file; do
  if is_code "$file"; then
    echo "code change: $file" >&2
    code=true
  elif [[ "$file" == examples/*/* ]]; then
    echo "example change: $file" >&2
    examples=true
  fi
done < <(gh api --paginate "repos/$GITHUB_REPOSITORY/pulls/$PR_NUMBER/files" -q '.[].filename')
echo "code=$code"
echo "examples=$examples"
