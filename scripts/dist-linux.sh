#!/usr/bin/env bash

set -euo pipefail

profile_dir="${1:?profile_dir is required}"

version="$(
  cargo metadata --locked --no-deps --format-version 1 |
    jq -er '.packages[] | select(.name == "codex-switch") | .version'
)"

arch="$(uname -m)"
case "$arch" in
  arm64) arch="aarch64" ;;
esac

output="dist/codex-switch-$version-linux-$arch.tar.gz"
mkdir -p dist
tar -C "$profile_dir" -czf "$output" codex-switch
echo "created $output"
