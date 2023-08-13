{
  description = "lively-rs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    nixgl.url = "github:guibou/nixGL";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
  };

  outputs = {
    nixgl,
    nixpkgs,
    ...
  } @ inputs: let
    pkgs = import nixpkgs {
      system = "x86_64-linux";
      overlays = [nixgl.overlay];
    };
  in
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
            glxinfo
          ];

          RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
        };

        packages =
          {
            lively = pkgs.rustPlatform.buildRustPackage {
              pname = "wgpu";
              version = "0.1.0";

              src = ./.;

              cargoLock = {
                lockFile = ./Cargo.lock;
              };

              nativeBuildInputs = with pkgs; [pkg-config];
              buildInputs = with pkgs; [
                glxinfo
                libinput
                libxkbcommon
                udev
                wayland
                vulkan-loader
                vulkan-validation-layers
                vulkan-headers
                vulkan-tools
                mesa.drivers
                libinput
              ];
              LD_LIBRARY_PATH = "/run/opengl-driver/lib:/run/opengl-driver/32/lib";
            };
          }
          // {default = config.packages.lively;};

        formatter = pkgs.alejandra;
      };
    };
}
