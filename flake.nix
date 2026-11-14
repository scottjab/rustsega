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

        # The workspace version, which is what `rustsega --version` prints. Read
        # from Cargo.toml so the package, the binary and the smoke check below
        # cannot drift apart (#160).
        workspaceVersion =
          (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        # What the single-binary derivation copies into the store (#160): the
        # workspace manifests and every crate. A fileset lists what goes in
        # rather than what stays out, which is what keeps `target/` (22 GB in a
        # warm checkout) and `test-roms/` out of the store without a filter to
        # keep honest, and it needs no `git` at eval time.
        #
        # `crates/web/dist` is the one part of `crates/` that is not source: it
        # is what trunk produces, and this derivation produces it too.
        sourceFileset = pkgs.lib.fileset.difference ./crates (pkgs.lib.fileset.maybeMissing ./crates/web/dist);

        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            sourceFileset
          ];
        };

        # cargo and rustc are the dev shell's own toolchain: the rust-overlay one
        # that `rust-toolchain.toml` pins, which is the one that carries the
        # `wasm32-unknown-unknown` target the frontend needs. `pkgs.rustPlatform`
        # would be a second, unpinned rustc, without it.
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rust;
          rustc = rust;
        };

        # The single binary (M8-05, #160): the frontend built by trunk and baked
        # into the server by `embed-web`, in one derivation and with no network.
        rustsega = rustPlatform.buildRustPackage {
          pname = "rustsega";
          version = workspaceVersion;
          inherit src;

          # One lockfile covers both halves of the build: `web` and `server` are
          # members of the same workspace, so the crates only the wasm frontend
          # uses are in here too. There are no git dependencies, which is what
          # keeps `cargoLock.outputHashes` out of this file.
          cargoLock.lockFile = ./Cargo.lock;

          # The three tools of the trunk build, all from nixpkgs. They have to
          # come from PATH: `crates/web/Trunk.toml` sets `offline = true` because
          # NixOS cannot exec the binaries trunk would otherwise download, and it
          # pins the two versions trunk checks these against.
          nativeBuildInputs = [ pkgs.trunk pkgs.wasm-bindgen-cli pkgs.binaryen ];

          # Step 1 of 2, the frontend, into `crates/web/dist` in the build tree.
          # It is a `preBuild` because `embed-web` reads that directory at compile
          # time, so the cargo build below needs it to already be there.
          preBuild = ''
            # trunk wants a home for its tool cache and a sandbox has no writable
            # one; $TMPDIR is the directory stdenv made for exactly this.
            export HOME=$TMPDIR
            (cd crates/web && trunk build --release --offline)
          '';

          # Step 2 of 2: the default cargo build, narrowed to the binary that
          # ships. What is left out of that choice is `headless` and `testkit`,
          # which are test tooling: every other crate of the workspace is either
          # a dependency of `server` or part of the wasm build above.
          cargoBuildFlags = [ "-p" "server" ];
          buildFeatures = [ "embed-web" ];

          # Tests run in `nix develop -c just check` and in CI, where the test
          # data, the ROM directories, nextest and Firefox all are. In here they
          # would need all of those, and what this derivation has to prove is the
          # packaging, not the emulator.
          doCheck = false;

          meta = {
            description = "Sega Genesis / Mega Drive emulator, serving the WASM app from one binary";
            homepage = "https://github.com/scottjab/rustsega";
            mainProgram = "rustsega";
          };
        };
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
            # SVG rasteriser, to regenerate the PWA icons of M7-18 (#151) from
            # `crates/web/assets/icon.svg` (see the comment in that file for the
            # exact commands). The PNGs are committed, so this is only needed
            # when the icon art changes.
            pkgs.resvg
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
            # RUSTSEGA_TEST_ROM_DIR is deliberately not defaulted here.
            # testkit::test_rom_dir finds test-roms itself: the nearest one at or
            # above the current directory, then the checkout that owns a linked
            # worktree. A $PWD-based default cannot do that -- a worktree has no
            # test-roms of its own, and pointing every data-driven test at one
            # that does not exist made `just check` fail in worktrees (#236).
            # Set the variable to override discovery; nix develop inherits it.
            # Point wasm-bindgen-test-runner at the geckodriver this shell pins,
            # so browser tests use the same driver version on every machine and
            # never pick up one off $PATH. Unlike the ROM dirs this is not
            # overridable: a driver and a browser that disagree on the WebDriver
            # protocol produce failures that look like emulator bugs.
            export GECKODRIVER="${pkgs.geckodriver}/bin/geckodriver"
          '';
        };

        packages = {
          # The same derivation under both names, so `nix build` and
          # `nix build .#rustsega` produce one store path.
          inherit rustsega;
          default = rustsega;
        };

        # `nix run` starts the server. The `meta` is not decoration: `nix flake
        # check` reads it for every app of a flake and warns when there is none.
        apps.default = (flake-utils.lib.mkApp { drv = rustsega; }) // {
          meta.description = "Serve the rustsega web app from the built binary";
        };

        # The packaging tests (M8-05, #160). There is no Rust test for this: the
        # thing to prove is that `nix build` produces a working single binary,
        # and that is a property of the derivation, so it is tested by building
        # it. `nix flake check` runs both of these.
        checks = {
          # Building the default package is the assertion; it has no output.
          package = self.packages.${system}.default;

          # And the binary that came out of it has to run with nothing else on
          # disk. `--version` and `--help` are answered before the server reads
          # a directory, a socket or an environment variable, so they need no
          # sandbox of their own and still catch a binary that is missing,
          # unrunnable, or built from another workspace.
          smoke = pkgs.runCommand "rustsega-smoke" { } ''
            bin=${self.packages.${system}.default}/bin/rustsega

            # The version comes from the workspace Cargo.toml, so this is what
            # says the binary in the store is the one this flake just built.
            version="$($bin --version)"
            echo "--version: $version"
            case "$version" in
              "rustsega ${workspaceVersion}") ;;
              *)
                echo "expected '--version' to print 'rustsega ${workspaceVersion}'" >&2
                exit 1
                ;;
            esac

            help="$($bin --help)"
            echo "$help"
            for flag in --bind --port --rom-dir --log; do
              case "$help" in
                *"$flag"*) ;;
                *)
                  echo "expected '--help' to document $flag" >&2
                  exit 1
                  ;;
              esac
            done

            touch $out
          '';
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
