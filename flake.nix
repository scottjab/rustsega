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
            # For linting the shell scripts in scripts/ (`shellcheck scripts/*.sh`).
            pkgs.shellcheck
            pkgs.curl
            pkgs.unzip
            pkgs.pkg-config
            # For the web crate's wasm-bindgen browser tests (see M7-20).
            # `wasm-bindgen-test-runner`, already here as part of
            # `wasm-bindgen-cli`, drives a real browser through WebDriver: it
            # finds the driver with the GECKODRIVER variable below and runs
            # headless unless NO_HEADLESS is set, so no display is needed.
            # Firefox rather than Chrome: geckodriver is maintained by Mozilla
            # and nixpkgs builds it against a known Firefox, so the pair works
            # together without any version hunting on our side.
            # Pinned through flake.lock like everything else: firefox 159.0.1 and
            # geckodriver 0.37.1 in the revision this flake.lock points at.
            pkgs.firefox
            pkgs.geckodriver
          ];

          # Where the emulator tests look for commercial ROMs. Override if needed.
          shellHook = ''
            export RUSTSEGA_ROM_DIR="''${RUSTSEGA_ROM_DIR:-$HOME/games/roms/genesis}"
            export RUSTSEGA_TEST_ROM_DIR="''${RUSTSEGA_TEST_ROM_DIR:-$PWD/test-roms}"
            # Point wasm-bindgen-test-runner at the geckodriver this shell pins,
            # so browser tests use the same driver version on every machine and
            # never pick up one off $PATH. Unlike the ROM dirs this is not
            # overridable: a driver and a browser that disagree on the WebDriver
            # protocol produce failures that look like emulator bugs.
            export GECKODRIVER="${pkgs.geckodriver}/bin/geckodriver"
          '';
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
