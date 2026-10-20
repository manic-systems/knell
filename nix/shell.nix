{
  lib,
  stdenv,
  mkShell,
  rustc,
  cargo,
  rust-analyzer,
  rustfmt,
  clippy,
  taplo,
  clang,
  wild,
  rustPlatform,
  extraPackages ? [],
}: let
  hasWild =
    stdenv.hostPlatform.isLinux && (stdenv.hostPlatform.isx86_64 || stdenv.hostPlatform.isAarch64);
in
  mkShell {
    name = "knell";

    strictDeps = true;

    nativeBuildInputs =
      [
        rustc
        cargo
        rust-analyzer
        rustfmt
        clippy
        taplo
      ]
      ++ lib.optionals hasWild [
        clang
        wild
      ]
      ++ extraPackages;

    env =
      {
        RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";
      }
      // lib.optionalAttrs hasWild {
        RUSTFLAGS = "-Clinker=${clang}/bin/clang -Clink-arg=--ld-path=wild";
      };
  }
