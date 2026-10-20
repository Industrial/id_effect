#!/usr/bin/env bash
# Workflow policy for .github/workflows (0.11.0.md S1, S2). Fails when a workflow:
#   - uses an action or reusable workflow that is not pinned by a full 40-character commit SHA
#     with a `# vX.Y.Z` comment (Dependabot bumps both). Local `./` actions are exempt;
#     `docker://` images must carry an `@sha256:` digest; slsa-framework/slsa-github-generator
#     reusable workflows are the one tag-pinned exception (`@vX.Y.Z`): its builder verifies the
#     ref it was called by and refuses a SHA;
#   - has no top-level `permissions:`, or a top-level grant other than read/none (`write-all`,
#     `read-all`, `<scope>: write`). Job-level write grants are fine;
#   - pipes a download into a shell (`curl … | sh`, `wget … | bash`, `bash <(curl …)`);
#   - approves a pull request (`gh pr review --approve`, an auto-approve action): a workflow must
#     never approve its own (Dependabot's) PRs.
#
# Usage: bash scripts/check-workflows.sh [--self-test] [FILE...]
#   no FILE: every .github/workflows/*.yml and *.yaml. --self-test runs the s1_/s2_ cases below
#   against fixtures in a temporary directory (and, for s2_dependabot_does_not_self_approve, the
#   repository's dependabot-auto-merge.yml).
# Requires: bash, a POSIX awk (AWK=… picks another; gawk, mawk and busybox awk all work).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# One file per run; prints `file:line: problem` lines and exits 1 if there is any.
AWK_PROG="$(
    cat <<'AWK'
function fail(line, msg) { printf "%s:%d: %s\n", FILENAME, line, msg; bad++ }
function strip_comment(s) { sub(/[ \t]+#.*$/, "", s); return s }
function check_grant(line, v) {
    if (v ~ /write/) fail(line, "top-level permissions grant write (" v "); grant it on the job that needs it")
    else if (v ~ /read-all/) fail(line, "top-level permissions: read-all; list the scopes (contents: read) or use {}")
}
BEGIN { bad = 0; top = 0; in_perm = 0; buf = ""; buf_line = 0 }
{
    raw = $0
    sub(/\r$/, "", raw)

    # S2: the top-level permissions block.
    if (in_perm) {
        if (raw ~ /^[^ \t#]/) in_perm = 0
        else if (raw ~ /^[ \t]+[A-Za-z-]+[ \t]*:/) check_grant(NR, strip_comment(raw))
    }
    if (raw ~ /^permissions[ \t]*:/) {
        top = NR
        v = strip_comment(raw)
        sub(/^permissions[ \t]*:[ \t]*/, "", v)
        if (v == "") in_perm = 1
        else check_grant(NR, v)
    }

    if (raw ~ /^[ \t]*#/) next

    # S1: every `uses:` pinned by full commit SHA, with a version comment.
    if (raw ~ /^[ \t]*(-[ \t]+)?uses[ \t]*:/) {
        v = raw
        sub(/^[ \t]*(-[ \t]+)?uses[ \t]*:[ \t]*/, "", v)
        comment = ""
        if (match(v, /[ \t]+#/)) { comment = substr(v, RSTART); v = substr(v, 1, RSTART - 1) }
        gsub(/["']/, "", v)
        sub(/[ \t]+$/, "", v)
        if (v ~ /^\.\.?\//) {
            # local action or reusable workflow in this repository
        } else if (v ~ /^docker:\/\//) {
            if (v !~ /@sha256:[0-9a-f]+$/) fail(NR, "docker action not pinned by digest: " v)
        } else if (v ~ /^slsa-framework\/slsa-github-generator\/\.github\/workflows\/[^@]+@v[0-9]+\.[0-9]+\.[0-9]+$/) {
            # tag-pinned by design (see the header)
        } else {
            at = 0
            for (i = length(v); i > 0; i--) if (substr(v, i, 1) == "@") { at = i; break }
            ref = at ? substr(v, at + 1) : ""
            if (!(length(ref) == 40 && ref ~ /^[0-9a-f]+$/)) fail(NR, "action not pinned by a full commit SHA: " v)
            else if (comment !~ /#[ \t]*[^ \t]/) fail(NR, "SHA-pinned action without a `# vX.Y.Z` comment: " v)
        }
    }

    # S2: no workflow approves a pull request.
    if (raw ~ /gh[ \t]+pr[ \t]+review/ && raw ~ /(--approve|[ \t]-a)([ \t"']|$)/) fail(NR, "workflow approves a pull request (gh pr review --approve)")
    if (raw ~ /uses[ \t]*:.*auto-approve/) fail(NR, "workflow approves a pull request (auto-approve action)")

    # S1: no download piped into a shell (shell continuation lines joined first).
    if (raw ~ /\\[ \t]*$/) {
        if (buf == "") buf_line = NR
        line = raw
        sub(/\\[ \t]*$/, "", line)
        buf = buf line " "
        next
    }
    line = buf raw
    ln = buf == "" ? NR : buf_line
    buf = ""
    if (match(line, /(curl|wget)[ \t]/)) {
        rest = substr(line, RSTART)
        if (rest ~ /\|[ \t]*(sudo[ \t]+)?(ba|z|da|k)?sh([ \t;&)]|$)/) fail(ln, "download piped into a shell (curl | sh); download a pinned release and check its sha256")
    }
    if (line ~ /(ba|z|da|k)?sh[ \t]+(-[A-Za-z]+[ \t]+)*<\([ \t]*(curl|wget)/) fail(ln, "download piped into a shell (sh <(curl ...)); download a pinned release and check its sha256")
}
END {
    if (!top) { printf "%s: no top-level permissions: (use contents: read or {})\n", FILENAME; bad++ }
    exit (bad > 0 ? 1 : 0)
}
AWK
)"

check_file() {
    "${AWK:-awk}" "$AWK_PROG" "$1"
}

check_files() {
    local status=0 f
    for f in "$@"; do
        check_file "$f" || status=1
    done
    return "$status"
}

self_test() {
    local tmp pass=0
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN

    write() { cat >"$tmp/$1"; }
    expect_pass() {
        local case="$1" file="$2" out
        if ! out="$(check_file "$file" 2>&1)"; then
            echo "FAIL $case: expected $(basename "$file") to pass, got:" >&2
            echo "$out" >&2
            return 1
        fi
        pass=$((pass + 1))
    }
    expect_fail() {
        local case="$1" file="$2" want="$3" out
        if out="$(check_file "$file" 2>&1)"; then
            echo "FAIL $case: expected $(basename "$file") to fail ($want)" >&2
            return 1
        fi
        if ! grep -qF -- "$want" <<<"$out"; then
            echo "FAIL $case: $(basename "$file") failed without \"$want\":" >&2
            echo "$out" >&2
            return 1
        fi
        pass=$((pass + 1))
    }

    # ── s1_every_action_pinned_by_sha ────────────────────────────────────────────────────────
    write pinned.yml <<'YML'
name: ok
on: push
permissions:
  contents: read
jobs:
  a:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - name: Quoted
        uses: "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6" # v2.9.2
      - uses: ./.github/actions/local
      - uses: docker://alpine@sha256:4bcff63911fcb4448bd4fdacec207030997caf25e9bea4045fa6c8c44de311d1
      - name: Pinned download
        run: |
          curl -fsSLo moon.tar.xz https://example.com/moon.tar.xz
          echo "abc  moon.tar.xz" | sha256sum -c -
  slsa:
    permissions:
      id-token: write
    uses: slsa-framework/slsa-github-generator/.github/workflows/generator_generic_slsa3.yml@v2.1.0
YML
    expect_pass s1_every_action_pinned_by_sha "$tmp/pinned.yml"
    local bad_uses
    for bad_uses in 'actions/checkout@v7' 'actions/checkout@3d3c42e' 'dtolnay/rust-toolchain@stable' \
        'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1' 'docker://alpine:3.20' \
        'org/repo/.github/workflows/x.yml@main'; do
        printf 'name: x\non: push\npermissions:\n  contents: read\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: %s\n' \
            "$bad_uses" | write unpinned.yml
        case "$bad_uses" in
            docker://*) expect_fail s1_every_action_pinned_by_sha "$tmp/unpinned.yml" "not pinned by digest" ;;
            *@3d3c42e5aac5ba805825da76410c181273ba90b1) expect_fail s1_every_action_pinned_by_sha "$tmp/unpinned.yml" "without a \`# vX.Y.Z\` comment" ;;
            *) expect_fail s1_every_action_pinned_by_sha "$tmp/unpinned.yml" "not pinned by a full commit SHA" ;;
        esac
    done
    local bad_run
    for bad_run in 'curl -fsSL https://moonrepo.dev/install/moon.sh | bash' \
        'wget -qO- https://x.example/install | sudo sh' \
        'curl -sSf https://sh.rustup.rs | tee /dev/null | sh -s -- -y' \
        'bash <(curl -fsSL https://x.example/install.sh)'; do
        printf 'name: x\non: push\npermissions:\n  contents: read\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: %s\n' \
            "$bad_run" | write piped.yml
        expect_fail s1_every_action_pinned_by_sha "$tmp/piped.yml" "download piped into a shell"
    done
    write piped_multiline.yml <<'YML'
name: x
on: push
permissions:
  contents: read
jobs:
  a:
    runs-on: ubuntu-latest
    steps:
      - run: |
          curl -fsSL \
            https://moonrepo.dev/install/moon.sh \
            | bash
YML
    expect_fail s1_every_action_pinned_by_sha "$tmp/piped_multiline.yml" "download piped into a shell"

    # ── s2_every_workflow_least_privilege ────────────────────────────────────────────────────
    write no_perms.yml <<'YML'
name: x
on: push
jobs:
  a:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - run: true
YML
    expect_fail s2_every_workflow_least_privilege "$tmp/no_perms.yml" "no top-level permissions"
    local bad_perms
    for bad_perms in 'permissions: write-all' 'permissions: read-all' $'permissions:\n  contents: write' \
        $'permissions:\n  contents: read\n  pull-requests: write # comment' 'permissions: { contents: write }'; do
        printf 'name: x\non: push\n%s\njobs:\n  a:\n    runs-on: ubuntu-latest\n    steps:\n      - run: true\n' \
            "$bad_perms" | write top_write.yml
        expect_fail s2_every_workflow_least_privilege "$tmp/top_write.yml" "top-level permissions"
    done
    local good_perms
    for good_perms in 'permissions: {}' 'permissions: { contents: read }' $'permissions:\n  contents: read\n  actions: read'; do
        printf 'name: x\non: push\n%s\njobs:\n  a:\n    runs-on: ubuntu-latest\n    permissions:\n      contents: write\n      pages: write\n    steps:\n      - run: true\n' \
            "$good_perms" | write least.yml
        expect_pass s2_every_workflow_least_privilege "$tmp/least.yml"
    done

    # ── s2_dependabot_does_not_self_approve ──────────────────────────────────────────────────
    write approve.yml <<'YML'
name: x
on: pull_request
permissions: {}
jobs:
  a:
    runs-on: ubuntu-latest
    permissions:
      pull-requests: write
    steps:
      - name: Approve PR
        run: gh pr review --approve "$PR_URL"
YML
    expect_fail s2_dependabot_does_not_self_approve "$tmp/approve.yml" "approves a pull request"
    write approve_action.yml <<'YML'
name: x
on: pull_request
permissions: {}
jobs:
  a:
    runs-on: ubuntu-latest
    steps:
      - uses: hmarr/auto-approve-action@f0939ea97e9205ef24d872e76833fa908a770363 # v4.0.0
YML
    expect_fail s2_dependabot_does_not_self_approve "$tmp/approve_action.yml" "approves a pull request"
    expect_pass s2_dependabot_does_not_self_approve "$ROOT/.github/workflows/dependabot-auto-merge.yml"

    echo "check-workflows: self-test passed ($pass checks: s1_every_action_pinned_by_sha, s2_every_workflow_least_privilege, s2_dependabot_does_not_self_approve)"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit 0
fi

files=("$@")
if ((${#files[@]} == 0)); then
    shopt -s nullglob
    files=("$ROOT"/.github/workflows/*.yml "$ROOT"/.github/workflows/*.yaml)
    shopt -u nullglob
fi
if ((${#files[@]} == 0)); then
    echo "check-workflows: no workflow files found" >&2
    exit 2
fi

if ! check_files "${files[@]}"; then
    echo "check-workflows: policy violations above (0.11.0.md S1, S2)" >&2
    exit 1
fi
echo "check-workflows: ${#files[@]} workflow(s): actions SHA-pinned, top-level permissions least privilege, no piped installers, no self-approval"
