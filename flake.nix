{
  description = "CodeTracer PolkaVM Recorder";

  inputs = {
    mcl-blockchain.url = "github:metacraft-labs/nix-blockchain-development";
    nixpkgs.follows = "mcl-blockchain/nixpkgs";
    flake-utils.follows = "mcl-blockchain/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      mcl-blockchain,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        devShells.default = pkgs.mkShell {
          inputsFrom = [ mcl-blockchain.devShells.${system}.polkavm ];
          packages = [
            pkgs.zstd # required by libcodetracer_trace_writer (Nim FFI)
            # Declare the toolchain explicitly so CI's dev shell
            # mirrors local dev exactly.  Cached mcl-blockchain
            # devShells from Attic sometimes drop nim/nimble from
            # PATH on resolution; declaring them here keeps the
            # contract visible in flake.nix.
            pkgs.nim
            pkgs.nimble
            pkgs.just
            pkgs.capnproto
            pkgs.rustc
            pkgs.cargo
            pkgs.rustfmt
            pkgs.clippy
            pkgs.pkg-config
            pkgs.lld # links PolkaVM guests (test-programs/rust-guest)
          ];

          # `cargo <subcommand>` looks for `cargo-<subcommand>` in
          # `$CARGO_HOME/bin` BEFORE it searches PATH. On any machine with
          # rustup — including the self-hosted macOS runner — that directory
          # holds rustup's proxies, so `cargo fmt` and `cargo clippy` run
          # rustup's `cargo-fmt` / `cargo-clippy` instead of the shell's, and
          # fail with "'cargo-fmt' is not installed for the toolchain".
          #
          # The shell therefore gets its own CARGO_HOME with an empty `bin/`,
          # so subcommand lookup falls through to PATH. `registry/` and `git/`
          # are symlinks to the real CARGO_HOME, and so are its config and
          # credentials when present: the download cache is shared, and only
          # the proxy directory is left behind.
          # Rust's library source, so a PolkaVM guest builds `core` from
          # source (`cargo build -Zbuild-std`, test-programs/rust-guest):
          # nixpkgs' rustc has no rust-src in its sysroot, and cargo reads
          # the library from here instead.
          __CARGO_TESTS_ONLY_SRC_ROOT = "${pkgs.rustPlatform.rustLibSrc}";
          # The PolkaVM target links with `rust-lld`, which nixpkgs' rustc
          # does not ship; `ld.lld` is the same linker.
          CARGO_TARGET_RISCV32EMAC_UNKNOWN_NONE_POLKAVM_LINKER = "ld.lld";

          shellHook = ''
            _polkavm_real_cargo_home="''${CARGO_HOME:-$HOME/.cargo}"
            _polkavm_cargo_home="''${XDG_CACHE_HOME:-$HOME/.cache}/codetracer-polkavm-recorder/cargo-home"
            if [ "$_polkavm_real_cargo_home" != "$_polkavm_cargo_home" ]; then
              mkdir -p "$_polkavm_cargo_home" \
                "$_polkavm_real_cargo_home/registry" "$_polkavm_real_cargo_home/git"
              # Re-pointed on every entry, so a changed CARGO_HOME is followed
              # rather than left sharing the previous one's cache. Only a link
              # is ever replaced; a real file placed here is left alone.
              for _polkavm_entry in registry git config.toml credentials.toml; do
                if [ -e "$_polkavm_real_cargo_home/$_polkavm_entry" ] &&
                  { [ -L "$_polkavm_cargo_home/$_polkavm_entry" ] ||
                    [ ! -e "$_polkavm_cargo_home/$_polkavm_entry" ]; }; then
                  ln -sfn "$_polkavm_real_cargo_home/$_polkavm_entry" "$_polkavm_cargo_home/$_polkavm_entry"
                fi
              done
              export CARGO_HOME="$_polkavm_cargo_home"
            fi
            unset _polkavm_real_cargo_home _polkavm_cargo_home _polkavm_entry
          '';
        };
      }
    );
}
