{
  inputs,
  pkgs,
  ...
}: let
  pkgsWithRust = import inputs.nixpkgs {
    inherit (pkgs.stdenv.hostPlatform) system;
    overlays = [inputs.rust-overlay.overlays.default];
  };
  # Same pin as CI's moon-ci and coverage jobs: the `ui` trybuild suite is gated to rustc 1.99.0.
  rustStable = pkgsWithRust.rust-bin.stable."1.99.0".default;

  # Moon from GitHub releases (x86_64-linux). See https://moonrepo.dev/docs/install
  moon = pkgs.stdenv.mkDerivation {
    pname = "moon-cli";
    version = "2.1.3";
    src = pkgs.fetchurl {
      url = "https://github.com/moonrepo/moon/releases/download/v2.1.3/moon_cli-x86_64-unknown-linux-gnu.tar.xz";
      sha256 = "0ir2qh8rifgcmfyb4xyndf9b1yjbn1fzr1gblnj5bnmar99rs60r";
    };
    nativeBuildInputs = [pkgs.autoPatchelfHook];
    buildInputs = [pkgs.stdenv.cc.cc.lib];
    installPhase = ''
      runHook preInstall
      mkdir -p $out/bin
      install -m755 moon $out/bin/moon
      runHook postInstall
    '';
    meta = {
      description = "Moon CLI (moonrepo)";
      homepage = "https://moonrepo.dev";
      license = pkgs.lib.licenses.mit;
      platforms = pkgs.lib.platforms.linux;
    };
  };

  blaze = pkgs.stdenv.mkDerivation rec {
    pname = "blaze";
    version = "0.20261030.1";
    src = pkgs.fetchurl {
      url = "https://github.com/indexable-inc/blaze/releases/download/v${version}/blaze-v${version}-x86_64-unknown-linux-gnu.tar.gz";
      hash = "sha256-QSH+6gaAnrXU1vT7W/5C/dSSge1zrc/fqjpdbGYcbOQ=";
    };
    sourceRoot = "blaze-v${version}-x86_64-unknown-linux-gnu";
    nativeBuildInputs = [pkgs.autoPatchelfHook];
    buildInputs = [pkgs.stdenv.cc.cc.lib pkgs.zlib];
    dontStrip = true;
    installPhase = ''
      runHook preInstall
      mkdir -p $out/bin $out/libexec/blaze
      for f in blaze blaze-engine blaze-proc-macro-srv blaze-rustc; do
        if [ -f "$f" ]; then
          install -m755 "$f" "$out/libexec/blaze/$f"
          ln -s "../libexec/blaze/$f" "$out/bin/$f"
        fi
      done
      [ -e $out/bin/blaze ] || { echo "no blaze binary in the release" >&2; exit 1; }
      runHook postInstall
    '';
    meta = {
      description = "blaze: cargo's build, check, run and test from a daemon that keeps the workspace compiled";
      homepage = "https://ix.dev/blaze";
      license = with pkgs.lib.licenses; [mit asl20];
      platforms = ["x86_64-linux"];
      mainProgram = "blaze";
    };
  };

  # roam-code: architectural intelligence CLI (https://github.com/Cranot/roam-code)
  roam-code-src = pkgs.fetchFromGitHub {
    owner = "Cranot";
    repo = "roam-code";
    rev = "89bc4d4216dba1977f073323c32eeb7c7221ebe0";
    hash = "sha256-AE1SQaBO/Od1My/nIsH2XQkU2342GIosHf5PJN8NFPg=";
  };
  roam-code = pkgs.python3Packages.buildPythonApplication rec {
    pname = "roam-code";
    version = "10.0.1";
    src = roam-code-src;
    format = "pyproject";
    nativeBuildInputs = with pkgs.python3Packages; [setuptools wheel];
    propagatedBuildInputs = with pkgs.python3Packages; [
      click
      tree-sitter
      tree-sitter-language-pack
      networkx
    ];
    doCheck = false;
  };
in {
  name = "id_effect";

  dotenv = {
    enable = true;
  };

  packages = with pkgs; [
    cachix
    perl
    direnv
    prek
    lldb
    cargo-watch
    cargo-audit
    cargo-cyclonedx
    cargo-deny
    cargo-semver-checks
    cargo-llvm-cov
    cargo-nextest
    sccache
    mold
    git
    gh
    blaze
    moon
    roam-code
    actionlint
    alejandra
    beautysh
    biome
    deadnix
    taplo
    treefmt
    vulnix
    yamlfmt
    mdbook
    ast-grep
    # D12: the generated TypeScript client's tests (`deno check`, `deno test`; CI pins 2.7.9).
    deno
    rustStable
  ];

  services.postgres = {
    enable = true;
    initialDatabases = [{name = "id_effect";}];
    listen_addresses = "127.0.0.1";
    # initdb's superuser is $USER; DATABASE_URL (and CI) connect as `postgres`.
    # Runs once, when `devenv up` creates a fresh data directory.
    initialScript = ''
      DO $$
      BEGIN
        IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'postgres') THEN
          CREATE ROLE postgres WITH LOGIN SUPERUSER;
        END IF;
      END
      $$;
    '';
  };

  env = {
    DATABASE_URL = "postgresql://postgres@127.0.0.1:5432/id_effect";
    CARGO_TERM_COLOR = "always";
    MOON_TOOLCHAIN_FORCE_GLOBALS = "rust";
    NEXTEST_NO_TESTS = "pass";
    OPENSSL_NO_VENDOR = "1";
    RUST_STABLE_BIN = "${rustStable}/bin";
  };

  languages.rust = {
    enable = true;
    # Nightly + rustc-dev allows the id_effect_lint Dylint crate to access
    # rustc internals (rustc_private).  The Dylint crate carries no
    # rust-toolchain pin so it always uses whatever nightly devenv provides.
    channel = "nightly";
    # Pinned so bumping rust-overlay (for a new stable) doesn't silently move the nightly.
    version = "2026-07-10";
    components = [
      "cargo"
      "clippy"
      "rust-analyzer"
      "rustc"
      "rustc-dev"
      "rust-src"
      "rustfmt"
      "llvm-tools"
    ];
    targets = [];
  };

  scripts = {
    prek-install = {
      exec = ''
        prek install -q --overwrite
      '';
    };

    moon-sync = {
      exec = ''
        moon sync
      '';
    };

    pre-push = {
      exec = ''
        export MOON_TOOLCHAIN_FORCE_GLOBALS=rust
        export MOON_CONCURRENCY=1
        mkdir -p "$DEVENV_ROOT/tmp"
        export TMPDIR="$DEVENV_ROOT/tmp"
        bash scripts/ci-local.sh auto
        if [ -n "$RUST_STABLE_BIN" ] && [ -d "$RUST_STABLE_BIN" ]; then
          _nightly_sysroot="$(rustc --print sysroot)"
          _target="$(rustc -vV | sed -n 's/^host: //p')"
          _llvm_bin="$_nightly_sysroot/lib/rustlib/$_target/bin"
          export PATH="$RUST_STABLE_BIN:$PATH"
          export CARGO_TARGET_DIR="$DEVENV_ROOT/state/target-ci-stable"
          export CARGO_HOME="$DEVENV_ROOT/state/cargo-ci-stable"
          mkdir -p "$CARGO_TARGET_DIR" "$CARGO_HOME"
          export LLVM_COV="$_llvm_bin/llvm-cov"
          export LLVM_PROFDATA="$_llvm_bin/llvm-profdata"
        fi
        moon run :coverage :audit
      '';
    };
  };

  enterShell = ''
    mkdir -p "$DEVENV_ROOT/tmp"
    export TMPDIR="$DEVENV_ROOT/tmp"

    prek-install
    moon-sync

    mkdir -p "$HOME/.cache/sccache"
    chmod 755 "$HOME/.cache/sccache" 2>/dev/null || true
  '';
}
