# The NixOS module of the server (M8-06, #161): `services.rustsega`.
#
# `self` is this flake, and it is here for one reason: `package` defaults to the
# binary this repository builds, and a module has no other way to name it. That
# is why the module is `self: { config, lib, pkgs, utils, ... }: ...` rather
# than a plain module - `utils` is the NixOS helper set, used for the escaping of
# `ExecStart` - and why `flake.nix` applies `self` before exposing it as
# `nixosModules.default` and `nixosModules.rustsega`.
#
# Everything the unit passes on the command line is an option, and every option
# is a flag of the binary (`crates/server/src/cli.rs`, #156): the module sets no
# environment variables, so `nixos-rebuild` shows the whole configuration in
# `systemctl cat rustsega`.
self:
{ config, lib, pkgs, utils, ... }:
let
  cfg = config.services.rustsega;

  # The components of the ROM directory, absolute and with no empty parts, so
  # the first of them is the top-level directory it lives under.
  romDirComponents =
    if cfg.romDir == null then
      [ ]
    else
      lib.filter (part: part != "") (lib.splitString "/" (toString cfg.romDir));

  # Whether the ROM directory is somewhere `ProtectHome` would hide it. The
  # hardening of the unit keeps `/home` off the table, which is only wrong when
  # the owner said that is where the ROMs are; and even then the directory is
  # only ever read, which is what `read-only` says.
  #
  # By components rather than by string prefix, because `/home-ware/roms` is not
  # under `/home`. `romDir` is stringified because `types.path` leaves a string
  # written to a path option a string.
  romDirInHome = romDirComponents != [ ] && lib.head romDirComponents == "home";
in
{
  options = {
    services.rustsega = {
      enable = lib.mkEnableOption "rustsega, the Sega Genesis emulator server";

      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.system}.default;
        defaultText = lib.literalExpression "self.packages.\${pkgs.system}.default";
        example = lib.literalExpression "self.packages.\${pkgs.system}.static";
        description = ''
          The `rustsega` package to serve: a binary with the web frontend
          embedded in it (#155). The default is this flake's own
          {option}`packages.default`; {option}`packages.static` is the same
          server built for a machine with no Nix on it (#159).
        '';
      };

      bind = lib.mkOption {
        type = lib.types.str;
        default = "127.0.0.1";
        example = "0.0.0.0";
        description = ''
          The address to bind, passed to the server as `--bind`.

          Loopback by default, because the server has no authentication of any
          kind (#156): everything on `port` can play every ROM in
          {option}`services.rustsega.romDir`, so this is a network decision and
          {option}`services.rustsega.openFirewall` is the other half of it.
          Use `0.0.0.0` to answer the whole network.
        '';
      };

      port = lib.mkOption {
        type = lib.types.port;
        default = 8080;
        description = ''
          The port to bind, passed to the server as `--port`.

          The unit runs as a dynamic user, so it cannot bind a privileged port
          below 1024: put nginx or another reverse proxy in front of this one
          for that.
        '';
      };

      romDir = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        example = "/srv/roms/genesis";
        description = ''
          The directory of ROMs to offer the frontend, passed to the server as
          `--rom-dir`; unset runs the server with no library (#157).

          The server checks the directory before it opens the socket, so it has
          to exist when the unit starts. The unit is given it read-only, and it
          runs as a dynamic user, so the files have to be world-readable or
          group-readable through a group the user is given;
          `/home/alice/roms` is the collection case that fails, because `alice`
          has no reason to have made it readable by anyone else. Put the
          collection under `/srv/roms` where this is normal, or bind mount the
          directory to somewhere the service may read.
        '';
      };

      openFirewall = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Open {option}`services.rustsega.port` in the firewall.

          Only useful together with a `bind` address that answers from outside
          the machine, and only safe on a network you trust: nothing
          authenticates to this server.
        '';
      };

      logLevel = lib.mkOption {
        type = lib.types.str;
        default = "info";
        example = "debug";
        description = ''
          The `tracing` level, passed to the server as `--log`: one of `trace`,
          `debug`, `info`, `warn`, `error`, `off`. The startup line naming the
          library and how many ROMs were scanned is at `info`.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    networking.firewall.allowedTCPPorts = lib.mkIf cfg.openFirewall [ cfg.port ];

    systemd.services.rustsega = {
      description = "rustsega Sega Genesis emulator server";
      wantedBy = [ "multi-user.target" ];
      after = [
        "network-online.target"
      ];
      wants = [ "network-online.target" ];

      serviceConfig = {
        Type = "simple";
        Restart = "on-failure";

        # A user per service, allocated at start and thrown away at stop. It is
        # the ROM directory that decides what that buys: the server is the one
        # process on the machine that reads a large, valuable collection, and a
        # dynamic user is what keeps it from reading anything else.
        DynamicUser = true;

        # One string, not a list: a list of a unit option's value is one line
        # per element, and a service takes exactly one `ExecStart`. Escaped per
        # argument, so a `romDir` with a space in it is still one argument.
        ExecStart = utils.escapeSystemdExecArgs (
          [
            "${cfg.package}/bin/rustsega"
            "--bind"
            cfg.bind
            "--port"
            cfg.port
            "--log"
            cfg.logLevel
          ]
          ++ lib.optionals (cfg.romDir != null) [
            "--rom-dir"
            cfg.romDir
          ]
        );

        # The hardening of the ticket. All of it leaves the server doing what it
        # does: read a directory, serve bytes from the store, listen on one
        # socket.
        NoNewPrivileges = true;
        PrivateTmp = true;
        # Everything read-only but the API filesystems: the server writes
        # nothing, holding the frontend in the binary and the ROMs in `romDir`.
        ProtectSystem = "strict";
        # Read-only `/home` is the option that keeps a ROM directory under
        # `/home` usable at all; see `romDirInHome`.
        ProtectHome = if romDirInHome then "read-only" else true;
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
        ];
        SystemCallFilter = [ "@system-service" ];

        # Belt to the `ProtectSystem` braces: the ROM directory is never written.
        ReadOnlyPaths = lib.optionals (cfg.romDir != null) [ (toString cfg.romDir) ];
      };
    };
  };
}
