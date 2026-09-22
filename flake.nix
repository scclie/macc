{
  description = "macc - self-service matrix account portal";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
  let
    system = "x86_64-linux";
    pkgs = nixpkgs.legacyPackages.${system};
  in
  {
    packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
      pname = "macc";
      version = "0.1.0";
      src = self;
      cargoLock.lockFile = ./Cargo.lock;
      meta = {
        description = "self-service matrix account portal (create/reset password via the admin room)";
        mainProgram = "macc";
      };
    };

    devShells.${system}.default = pkgs.mkShell {
      packages = [ pkgs.cargo pkgs.rustc ];
    };

    nixosModules.macc = import ./module.nix {
      inherit pkgs;
      maccPkg = self.packages.${system}.default;
    };
  };
}