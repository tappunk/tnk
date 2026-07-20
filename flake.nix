{
  description = "Zero-trust sandbox for local inference and secure AI coding agent runtimes.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      system = "aarch64-darwin";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      packages.aarch64-darwin.default = pkgs.rustPlatform.buildRustPackage {
        pname = "tnk";
        version = "0.1.29";
        src = ./.;

        cargoLock = {
          lockFile = ./Cargo.lock;
        };

        nativeBuildInputs = with pkgs; [ installShellFiles ];

        meta = with pkgs.lib; {
      description = "Zero-trust sandbox for local inference and secure AI coding agent runtimes.";
          homepage = "https://tappunk.com";
          license = licenses.asl20;
          maintainers = [ ];
          platforms = [ "aarch64-darwin" ];
          mainProgram = "tnk";
        };
      };

      devShells.aarch64-darwin.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          rustc
          cargo
          clippy
          rustfmt
          rust-analyzer
        ];

        shellHook = ''
          echo "tnk dev environment loaded (aarch64-darwin)"
        '';
      };

      apps.aarch64-darwin.default = {
        type = "app";
        program = "${self.packages.aarch64-darwin.default}/bin/tnk";
      };
    };
}
