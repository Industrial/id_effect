{
  inputs,
  pkgs,
  ...
}: let
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
    cargo-llvm-cov
    cargo-nextest
    sccache
    mold
    git
    gh
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
  ];

  env = {
    CARGO_TERM_COLOR = "always";
    MOON_TOOLCHAIN_FORCE_GLOBALS = "rust";
    NEXTEST_NO_TESTS = "pass";
    OPENSSL_NO_VENDOR = "1";
  };

  languages.rust = {
    enable = true;
    # Nightly + rustc-dev allows the id_effect_lint Dylint crate to access
    # rustc internals (rustc_private).  The Dylint crate carries no
    # rust-toolchain pin so it always uses whatever nightly devenv provides.
    channel = "nightly";
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
        moon run :format :check :build :test :coverage :audit :check-docs
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
