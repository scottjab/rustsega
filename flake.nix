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

        # --- portable static binary (M8-05b, #159) ---------------------------

        # The cargo target of the portable build. `x86_64-unknown-linux-musl`
        # links the C library in by default (`crt-static`), which is the whole
        # trick: one file, no `/nix/store`, no glibc to agree with.
        muslTarget = "x86_64-unknown-linux-musl";

        # Only x86_64-linux has the package and its check (#159); macOS, Windows
        # and aarch64 are out of scope.
        supportsStatic = pkgs.stdenv.hostPlatform.isLinux && pkgs.stdenv.hostPlatform.isx86_64;

        # The pinned toolchain of `rust-toolchain.toml` plus the musl target. A
        # `rust.override` replaces the target list rather than adding to it, so
        # `wasm32` is repeated here, and it is this package alone that pays for
        # the extra target: the dev shell keeps the file as it is.
        rustMusl = rust.override {
          targets = [ "wasm32-unknown-unknown" muslTarget ];
        };

        rustMuslPlatform = pkgs.makeRustPlatform {
          cargo = rustMusl;
          rustc = rustMusl;
        };

        # rustc links with `cc`, and this package set's `cc` is the glibc one, so
        # the link step of the static build is pointed at a musl gcc instead.
        # `cargo tree -p server -e normal` carries no `-sys` crate, so libc is
        # the only C in this build and a linker is all it takes - no C dependency
        # is added, and nothing is compiled with it.
        muslCc = pkgs.pkgsCross.musl64.stdenv.cc;

        # The portable single binary (M8-05b, #159): `rustsega` above built for
        # `muslTarget`, so that it runs on a Linux box with no nix on it.
        #
        # The cargo hooks of a rust platform build and install that platform's
        # own target - here `--target x86_64-unknown-linux-gnu`, read back from
        # `target/release` - so the two phases that touch the target are written
        # out instead. Everything else is the machinery of #160: the same `src`,
        # the same crates vendored from `Cargo.lock` by `cargoLock`, the same
        # strip in `fixupPhase`.
        #
        # The frontend is built here rather than shared with `rustsega` because
        # `crates/web/dist` is not an output of that derivation, so there is
        # nothing in the store to share. What is shared is that both bake in the
        # same tree: the wasm half of the build does not depend on the target.
        rustsegaStatic = rustMuslPlatform.buildRustPackage {
          pname = "rustsega-static";
          version = workspaceVersion;
          inherit src;

          cargoLock.lockFile = ./Cargo.lock;

          # The three tools of the trunk step, as in `rustsega`, and the C linker
          # of the musl link step. `muslCc` has to be an input, not just a path in
          # a variable: only an input puts it in the sandbox.
          nativeBuildInputs = [
            pkgs.trunk
            pkgs.wasm-bindgen-cli
            pkgs.binaryen
            muslCc
          ];

          # The link step only, and only for the musl target, so build scripts
          # and everything else keep this package set's own `cc`.
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER =
            "${muslCc}/bin/${muslCc.targetPrefix}cc";

          # Step 1 of 2, as in `rustsega`, and step 2 for one target only: the
          # hook would pass this platform's glibc target as well, doubling a
          # release build that has to be small enough to wait for.
          buildPhase = ''
            runHook preBuild

            # trunk wants a home for its tool cache and a sandbox has no writable
            # one; $TMPDIR is the directory stdenv made for exactly this.
            export HOME=$TMPDIR
            (cd crates/web && trunk build --release --offline)

            cargo build --release --offline -p server --features embed-web \
              --target ${muslTarget}

            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            install -Dm555 target/${muslTarget}/release/rustsega $out/bin/rustsega
            runHook postInstall
          '';

          # As in `rustsega`: the tests need the test data, the ROM directories
          # and nextest, and what has to be proven here is the packaging.
          doCheck = false;

          meta = {
            description = "Sega Genesis / Mega Drive emulator, statically linked for a Linux box with no nix";
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

          # The portable binary of M8-05b (#159), x86_64-linux only. A null
          # attribute name drops the attribute, which is how this stays out of
          # `nix flake check` on the systems that have no musl target.
          ${if supportsStatic then "static" else null} = rustsegaStatic;
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

          # The portable binary test (M8-05b, #159). The point of the static
          # build is that it carries no dynamic dependencies at all, and that is
          # a property of the binary rather than of the machine running it, so
          # `file` and `ldd` are the test: no container, no second host.
          # `glibc.bin` is `ldd`: a minimal build environment has no other, and
          # an absent `ldd` would read here as "no shared library needed".
          ${if supportsStatic then "static" else null} =
            pkgs.runCommand "rustsega-static-check"
            { nativeBuildInputs = [ pkgs.file pkgs.glibc.bin ]; } ''
            bin=${self.packages.${system}.static}/bin/rustsega

            # The two failures this check exists for are the glibc build, which
            # `file` calls "dynamically linked (uses shared libs)", and a musl
            # build that lost `crt-static`, which it calls "interleaved". A
            # correct one is "static-pie linked" here, and the phrasing of that
            # varies between versions of `file`, so the word static has to be
            # present and the words dynamically linked must not be.
            info="$(file -L "$bin")"
            echo "$info"
            case "$info" in
              *"dynamically linked"* | *interleaved*)
                echo "expected $bin to be statically linked" >&2
                exit 1
                ;;
              *static*) ;;
              *)
                echo "expected '$info' to say how the binary is linked" >&2
                exit 1
                ;;
            esac

            # And `ldd` agrees. A static binary gets "not a dynamic executable"
            # or "statically linked" depending on the `ldd` in hand, and the first
            # of those comes with exit status 1, so the status says nothing here;
            # what matters is that no shared library is resolved for the binary,
            # which is what an `=>` line would be.
            dynamic="$(ldd "$bin" 2>&1 || true)"
            echo "$dynamic"
            case "$dynamic" in
              *"=>"*)
                echo "expected $bin to need no shared library, ldd said:" >&2
                echo "$dynamic" >&2
                exit 1
                ;;
            esac

            # The version comes from the workspace Cargo.toml, so this is what
            # says the static binary is this workspace's server and not just any
            # binary that happens to be static.
            version="$("$bin" --version)"
            echo "--version: $version"
            case "$version" in
              "rustsega ${workspaceVersion}") ;;
              *)
                echo "expected '--version' to print 'rustsega ${workspaceVersion}'" >&2
                exit 1
                ;;
            esac

            touch $out
          '';
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
