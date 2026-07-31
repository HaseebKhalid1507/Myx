{
  description = "Myx - a terminal Spotify player";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          inherit (pkgs) lib;
        in
        rec {
          myx = pkgs.rustPlatform.buildRustPackage {
            pname = "myx";
            version = "0.3.0";

            src = lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              pkgs.pkg-config
            ];

            buildInputs =
              lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                pkgs.alsa-lib
                pkgs.openssl
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
                pkgs.libiconv
              ];

            strictDeps = true;

            meta = {
              description = "A lean, beautiful terminal Spotify player";
              homepage = "https://github.com/HaseebKhalid1507/Myx";
              license = lib.licenses.mit;
              mainProgram = "myx";
              platforms = supportedSystems;
            };
          };

          default = myx;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.myx}/bin/myx";
        };
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.myx ];
            packages = with pkgs; [
              cargo
              clippy
              rust-analyzer
              rustc
              rustfmt
            ];
          };
        }
      );
    };
}
