{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    naersk.url = "github:nix-community/naersk/master";
    naersk.inputs.nixpkgs.follows = "nixpkgs";
    utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      naersk,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        naersk-lib = pkgs.callPackage naersk { };
        tuitr = naersk-lib.buildPackage {
          src = ./.;
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; lib.optionals stdenv.isLinux [ xorg.libxcb ];
        };
      in
      {
        packages = {
          default = tuitr;
          inherit tuitr;
        };

        devShells.default =
          with pkgs;
          mkShell {
            buildInputs = [
              cargo
              rustc
              rustfmt
              rustPackages.clippy
              jj
              git
              ripgrep-all
              wl-clipboard
              xclip
            ];
            RUST_SRC_PATH = rustPlatform.rustLibSrc;
          };
      }
    )
    // {
      # Add to your nixconfig with:
      #   inputs.tuitr.url = "github:deadmade/tuitr";
      #   nixpkgs.overlays = [ inputs.tuitr.overlays.default ];
      # Then use pkgs.tuitr anywhere.
      overlays.default = final: _prev: {
        tuitr = self.packages.${final.system}.default;
      };
    };
}
