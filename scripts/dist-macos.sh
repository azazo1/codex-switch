#!/usr/bin/env bash

set -euo pipefail

profile_dir="${1:?profile_dir is required}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

version="$(bash "$script_dir/build-version.sh")"

arch="$(uname -m)"
case "$arch" in
  arm64) arch="aarch64" ;;
esac

output="dist/codex-switch-$version-macos-$arch.dmg"
bash "$script_dir/package-macos.sh" "$profile_dir" "$output"
