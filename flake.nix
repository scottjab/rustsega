{
  description = "rustsega: a Sega Genesis / Mega Drive emulator in Rust, running in the browser via WASM";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        # Toolchain is pinned in rust-toolchain.toml so cargo and nix agree.
        rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rust
            pkgs.trunk
            # IMPORTANT: the `wasm-bindgen` crate version in Cargo.toml must
            # exactly match this CLI version. Check with `wasm-bindgen --version`.
            pkgs.wasm-bindgen-cli
            pkgs.binaryen
            pkgs.cargo-nextest
            pkgs.cargo-insta
            pkgs.just
            pkgs.gh
            pkgs.git
            pkgs.ripgrep
            pkgs.jq
            pkgs.curl
            pkgs.unzip
            pkgs.pkg-config
          ];

          # Where the emulator tests look for commercial ROMs. Override if needed.
          shellHook = ''
            export RUSTSEGA_ROM_DIR="''${RUSTSEGA_ROM_DIR:-$HOME/games/roms/genesis}"
            export RUSTSEGA_TEST_ROM_DIR="''${RUSTSEGA_TEST_ROM_DIR:-$PWD/test-roms}"
          '';
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
