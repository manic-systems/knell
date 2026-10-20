{
  inputs = {
    nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.zst";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {self, ...} @ inputs: let
    inherit (inputs) nixpkgs fenix;
    inherit (nixpkgs) lib;
    forAllSystems = lib.genAttrs (lib.systems.doubles.linux ++ lib.systems.doubles.darwin);
    pkgsFor = system: nixpkgs.legacyPackages.${system} or (import nixpkgs {inherit system;});
  in {
    packages = forAllSystems (
      system: let
        knell = (pkgsFor system).callPackage ./nix/package.nix {};
      in {
        inherit knell;
        default = knell;
      }
    );

    devShells = forAllSystems (
      system: let
        pkgs = pkgsFor system;
      in {
        default = pkgs.callPackage ./nix/shell.nix {
          rustfmt = fenix.packages.${system}.latest.rustfmt or pkgs.rustfmt;
        };
      }
    );

    checks = forAllSystems (system: {
      knell = self.packages.${system}.knell;
    });

    formatter = forAllSystems (system: (pkgsFor system).alejandra);
  };
}
