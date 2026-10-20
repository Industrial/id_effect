#!/usr/bin/env bash
# Publish one crate to crates.io (.github/workflows/publish.yml). Safe to rerun after a partial
# publish: a version already in the crates.io index is skipped before cargo runs.
#
# Usage: publish-crate.sh <package> [extra cargo publish args...]
# Env:
#   VERSION               the version being published (required)
#   DRY_RUN=true          pass --dry-run to cargo publish
#   PLAN_ONLY=true        print "publish" or "skip" from the crates.io index and exit (read-only);
#                         a crate that was never published fails the plan unless DRY_RUN=true:
#                         trusted publishing cannot create a crate (docs/RELEASING.md)
#   SHA256SUMS            optional sha256sum file of the attested .crate files: the .crate that
#                         cargo publish uploaded must match its line
#   CRATES_IO_TOKEN       the short-lived trusted-publishing token (rust-lang/crates-io-auth-action);
#                         handed to cargo on stdin by `cargo login`, never on the command line
set -euo pipefail

pkg=$1
shift
version=${VERSION:?set VERSION to the version being published}

# Sparse-index path of a crate (https://doc.rust-lang.org/cargo/reference/registry-index.html).
index_url() {
    local name=${1,,}
    case ${#name} in
        1) echo "https://index.crates.io/1/$name" ;;
        2) echo "https://index.crates.io/2/$name" ;;
        3) echo "https://index.crates.io/3/${name:0:1}/$name" ;;
        *) echo "https://index.crates.io/${name:0:2}/${name:2:2}/$name" ;;
    esac
}

# 0 when $pkg $version is in the index. A crate never published is a 404: "not yet".
published() {
    local body
    body=$(curl -fsS --retry 3 --retry-delay 5 "$(index_url "$pkg")" 2>/dev/null) || return 1
    grep -qF "\"vers\":\"$version\"" <<<"$body"
}

# 0 when the crate has no version on crates.io at all (the index answers 404).
new_crate() {
    local status
    status=$(curl -sS -o /dev/null -w '%{http_code}' --retry 3 --retry-delay 5 "$(index_url "$pkg")")
    [ "$status" = "404" ]
}

if published; then
    echo "skip    $pkg $version (already on crates.io)"
    exit 0
fi
if [ "${PLAN_ONLY:-false}" = "true" ]; then
    if new_crate; then
        if [ "${DRY_RUN:-false}" != "true" ]; then
            echo "::error::$pkg has never been published: trusted publishing cannot create a crate. Reserve the name and add the trusted publisher first (docs/RELEASING.md, owner steps)."
            exit 1
        fi
        echo "publish $pkg $version (new crate: a real release needs the owner steps in docs/RELEASING.md)"
        exit 0
    fi
    echo "publish $pkg $version"
    exit 0
fi

args=(--package "$pkg" --no-verify "$@")
if [ "${DRY_RUN:-false}" = "true" ]; then
    args+=(--dry-run)
else
    : "${CRATES_IO_TOKEN:?set CRATES_IO_TOKEN (trusted publishing) for a real publish}"
    cargo login <<<"$CRATES_IO_TOKEN"
fi

set +e
output=$(cargo publish "${args[@]}" 2>&1)
code=$?
set -e
echo "$output"
if [ "$code" -ne 0 ]; then
    if grep -qiE "already (uploaded|exists)" <<<"$output"; then
        echo "$pkg already on crates.io — skipping"
        exit 0
    fi
    if grep -qiE "required permissions|403|not authorized|trusted publish" <<<"$output"; then
        echo "::error::$pkg publish denied — trusted publishing needs Industrial/id_effect publish.yml (environment crates-io) as a trusted publisher on this crate (docs/RELEASING.md)"
        exit 1
    fi
    if grep -qiE "readme .* does not appear to exist" <<<"$output"; then
        echo "::error::$pkg missing readme declared in Cargo.toml"
        exit 1
    fi
    if grep -qiE "no matching package named|didn't match|failed to select a version for the requirement" <<<"$output"; then
        echo "::error::$pkg publish blocked — dependency version not on crates.io yet (check publish level order / path-only dev-deps)"
        exit 1
    fi
    exit "$code"
fi

# The bytes cargo uploaded must be the attested ones. cargo publish leaves them in
# target/package/tmp-crate/ (current cargo) or target/package/ (older cargo).
if [ -n "${SHA256SUMS:-}" ]; then
    file="$pkg-$version.crate"
    dir="${CARGO_TARGET_DIR:-target}/package"
    uploaded="$dir/tmp-crate/$file"
    if [ ! -f "$uploaded" ]; then
        uploaded="$dir/$file"
    fi
    expected=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1 }' "$SHA256SUMS")
    actual=$(sha256sum "$uploaded" | cut -d' ' -f1)
    if [ "$expected" != "$actual" ]; then
        echo "::error::$file sha256 $actual is not the attested ${expected:-<missing>}"
        exit 1
    fi
    echo "$file matches the attested sha256 $actual"
fi

# cargo publish waits for the index itself; poll as well so a dependent never races it.
if [ "${DRY_RUN:-false}" != "true" ]; then
    for _ in $(seq 1 30); do
        if published; then
            exit 0
        fi
        sleep 10
    done
    echo "::error::$pkg $version is not in the crates.io index after 5 minutes"
    exit 1
fi
