self:
{ config, lib, pkgs, ... }:

let
  cfg = config.services.mprissence;
  toml = pkgs.formats.toml { };
  configFile = toml.generate "mprissence.toml" cfg.settings;
in
{
  options.services.mprissence = {
    enable = lib.mkEnableOption "mprissence, Discord rich presence for MPRIS media players";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "mprissence.packages.\${system}.default";
      description = "The mprissence package to use.";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = lib.literalExpression ''
        {
          client_id = "123456789012345678";
          ignore = [ "firefox" "chromium" ];
          show_paused = true;
        }
      '';
      description = ''
        Written to {file}`$XDG_CONFIG_HOME/mprissence/config.toml`.
        See config.example.toml for the available keys.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = cfg.settings ? client_id;
      message = "services.mprissence.settings.client_id must be set to a Discord application ID.";
    }];

    home.packages = [ cfg.package ];
    xdg.configFile."mprissence/config.toml".source = configFile;

    systemd.user.services.mprissence = {
      Unit = {
        Description = "Discord rich presence for MPRIS media players";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
        X-Restart-Triggers = [ "${configFile}" ];
      };
      Service = {
        ExecStart = lib.getExe cfg.package;
        Restart = "on-failure";
        RestartSec = 10;
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };
  };
}
