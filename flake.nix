{
  description = "Discord rich presence for MPRIS media players";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        mprissence = pkgs.rustPlatform.buildRustPackage {
          pname = "mprissence";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          meta = {
            description = "Discord rich presence for MPRIS media players";
            mainProgram = "mprissence";
            platforms = pkgs.lib.platforms.linux;
          };
        };
        default = mprissence;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = with pkgs; [ clippy rustfmt rust-analyzer ];
        };
      });

      homeManagerModules.default = import ./nix/hm-module.nix self;
    };
}
