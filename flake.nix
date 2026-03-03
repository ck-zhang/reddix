{
  description = "Reddix - Reddit, refined for the terminal";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.rustPlatform.buildRustPackage rec {
            pname = "reddix";
            version = "0.2.9";

            src = self;

            cargoHash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

            buildInputs = pkgs.lib.optional pkgs.stdenv.isDarwin pkgs.libiconv;

            meta = with pkgs.lib; {
              homepage = "https://github.com/ck-zhang/reddix";
              description = "Reddit, refined for the terminal";
              license = licenses.mit;
              mainProgram = "reddix";
            };
          };
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/reddix";
        };
      });
    };
}
