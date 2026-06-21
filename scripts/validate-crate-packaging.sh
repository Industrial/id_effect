#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

skip=id_effect_lint
cargo_dir=$(mktemp -d)
trap 'rm -rf "$cargo_dir"' EXIT
export CARGO_HOME="$cargo_dir"

mkdir -p "$CARGO_HOME"
{
  echo '[patch.crates-io]'
  while IFS= read -r toml; do
    crate=$(grep -m1 '^name = ' "$toml" | sed 's/^name = "\(.*\)"/\1/')
    [[ "$crate" == "$skip" ]] && continue
    dir=$(cd "$(dirname "$toml")" && pwd)
    echo "$crate = { path = \"$dir\" }"
  done < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml | sort)
} > "$CARGO_HOME/config.toml"

failed=0

while IFS= read -r toml; do
  crate=$(grep -m1 '^name = ' "$toml" | sed 's/^name = "\(.*\)"/\1/')
  [[ "$crate" == "$skip" ]] && continue

  if readme=$(grep -m1 '^readme = ' "$toml" | sed 's/^readme = "\(.*\)"/\1/'); then
    dir=$(dirname "$toml")
    if [[ ! -f "$dir/$readme" ]]; then
      echo "ERROR: $crate declares readme = \"$readme\" but $dir/$readme is missing"
      failed=1
      continue
    fi
  fi

  if ! cargo package --package "$crate" --no-verify --allow-dirty --quiet 2>/tmp/validate-pkg.err; then
    echo "ERROR: cargo package failed for $crate"
    sed 's/^/  /' /tmp/validate-pkg.err
    failed=1
  fi
done < <(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml | sort)

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "All publishable crates pass packaging validation"
