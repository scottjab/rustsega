# The NixOS test of the module (#161), as a `pkgs.testers.runNixOSTest` module.
#
# One machine, the service enabled with a ROM directory of one file, and three
# requests: what the module has to get right is the unit, the hardening around
# it and the command line it passes, and the answers on those three endpoints
# are what say all three worked. `crates/server/tests/` already covers what the
# server says; this covers what NixOS asks it to say.
#
# `module` is the module under test, `nixosModules.default` of this flake, so
# that this test drives the public output rather than the private binding.
{ module, pkgs }:
{
  name = "rustsega-nixos-module";

  nodes.machine =
    { ... }:
    {
      imports = [ module ];

      # `curl` for the three requests, `jq` to read two of the answers.
      environment.systemPackages = [
        pkgs.curl
        pkgs.jq
      ];

      services.rustsega = {
        enable = true;
        # The default, stated: the test is also what says loopback is what
        # `bind` means when nothing sets it.
        port = 8080;
        romDir = "/srv/roms";
      };

      # The one ROM, written during init so that the directory is there before
      # the service starts: an absent `--rom-dir` is a startup error of the
      # server, which is the right behaviour for a person and an obstacle for a
      # test that has to create the file itself.
      #
      # The bytes are not a cartridge. They are long enough to be a file rather
      # than nothing, which is all `Library::scan` asks of a `.md` (#157).
      systemd.tmpfiles.rules = [
        "f /srv/roms/test.md 0444 root root - rustsega nixos module test rom"
      ];
    };

  testScript = ''
    start_all

    machine.wait_for_unit("rustsega.service")
    machine.wait_for_open_port(8080)

    # `/healthz` is three bytes and says nothing about the ROM directory, so it
    # is the request that says the unit is up and the port is the one configured.
    health = machine.succeed("curl -fsS http://127.0.0.1:8080/healthz")
    assert health == "ok", "unexpected /healthz body: " + repr(health)

    # `"library":true` is what `--rom-dir` was passed: without the flag the
    # server answers this endpoint with `false` and every `/api/roms` 404s.
    info = machine.succeed("curl -fsS http://127.0.0.1:8080/api/info")
    assert '"library":true' in info, "unexpected /api/info body: " + repr(info)

    # One entry, and it is the file: the dynamic user of the unit read the
    # directory, which is what the `ProtectHome` and `ReadOnlyPaths` of the
    # module are about, and the scan named it the way the library does (#157).
    roms = "curl -fsS http://127.0.0.1:8080/api/roms | jq -e "
    machine.succeed(roms + "'length == 1'")
    machine.succeed(roms + "'.[0].name == \"test\"'")
    machine.succeed(roms + "'.[0].file == \"test.md\"'")
  '';
}
