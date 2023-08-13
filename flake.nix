{
  description = "lively-rs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      systems = ["x86_64-linux"];

      perSystem = {
        config,
        pkgs,
        system,
        ...
      }: {
        devShells.default = pkgs.mkShell {
          inputsFrom = [config.packages.lively];
          packages = with pkgs; [
            cargo
            clippy
            pre-commit
            rust-analyzer
            rustc
            rustfmt
            rustPackages.clippy
          ];

          RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
        };

        packages =
          {
            lively = pkgs.rustPlatform.buildRustPackage {
              pname = "lively";
              version = "0.1.0";

              src = ./.;

              cargoLock = {
                lockFile = ./Cargo.lock;
              };

              nativeBuildInputs = with pkgs; [pkg-config];
              buildInputs = with pkgs; [
                libinput
                libxkbcommon
                udev
                wayland
              ];
            };
          }
          // {default = config.packages.lively;};

        formatter = pkgs.alejandra;
      };
    };
}
