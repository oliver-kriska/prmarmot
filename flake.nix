{
  description = "PR Marmot — reproducible Rust and GPUI development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      supportedSystems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
      pkgsFor = forAllSystems (
        system:
        import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        }
      );
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor.${system};
          inherit (pkgs) lib;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          linuxLibraries = lib.optionals pkgs.stdenv.hostPlatform.isLinux (
            with pkgs;
            [
              clang
              fontconfig
              freetype
              libGL
              libxkbcommon
              vulkan-loader
              wayland
              xorg.libX11
              xorg.libXcursor
              xorg.libXi
              xorg.libXrandr
              xorg.libxcb
            ]
          );
          prmarmot = rustPlatform.buildRustPackage {
            pname = "prmarmot";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
            src = lib.cleanSource self;

            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = with pkgs; [
              clang
              makeWrapper
              pkg-config
            ];
            buildInputs = linuxLibraries;

            # Compile embedded shaders through Metal at launch so the Nix
            # build does not depend on an external Xcode Metal toolchain.
            buildFeatures = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
              "gpui_platform/runtime_shaders"
            ];

            # `nix flake check` runs the separate core test derivation.
            doCheck = false;

            postInstall = ''
              wrapProgram "$out/bin/prmarmot" \
                --prefix PATH : "${
                  lib.makeBinPath [
                    pkgs.coreutils
                    pkgs.gh
                  ]
                }"${lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                  \
                  --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath linuxLibraries}"
                ''}
            '';

            meta = {
              description = "GitHub PR review dashboard";
              homepage = "https://github.com/oliver-kriska/prmarmot";
              license = lib.licenses.mit;
              mainProgram = "prmarmot";
              platforms = supportedSystems;
            };
          };
        in
        {
          default = prmarmot;
          inherit prmarmot;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor.${system};
          inherit (pkgs) lib;
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          systemXcrun = pkgs.writeShellScriptBin "xcrun" ''
            original_args=("$@")
            find_only=0

            while [ "$#" -gt 0 ]; do
              case "$1" in
                -f|--find)
                  find_only=1
                  ;;
                metal|metallib)
                  tool="$1"
                  shift
                  toolchain_path="$(
                    /usr/bin/xcodebuild -showComponent metalToolchain 2>/dev/null \
                      | /usr/bin/awk -F ': ' '/^Toolchain Search Path:/ { print $2; exit }'
                  )"
                  candidate="$toolchain_path/Metal.xctoolchain/usr/bin/$tool"
                  if [ -x "$candidate" ]; then
                    if [ "$find_only" -eq 1 ]; then
                      echo "$candidate"
                      exit 0
                    fi
                    exec "$candidate" "$@"
                  fi
                  ;;
              esac
              shift
            done

            exec /usr/bin/xcrun "''${original_args[@]}"
          '';
          linuxLibraries = lib.optionals pkgs.stdenv.hostPlatform.isLinux (
            with pkgs;
            [
              clang
              fontconfig
              freetype
              libGL
              libxkbcommon
              vulkan-loader
              wayland
              xorg.libX11
              xorg.libXcursor
              xorg.libXi
              xorg.libXrandr
              xorg.libxcb
            ]
          );
        in
        {
          default = (if pkgs.stdenv.hostPlatform.isDarwin then pkgs.mkShellNoCC else pkgs.mkShell) {
            packages =
              with pkgs;
              [
                bashInteractive
                gh
                git
                nixfmt
                pkg-config
                rustToolchain
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ systemXcrun ];
            buildInputs = linuxLibraries;

            LIBCLANG_PATH = lib.optionalString pkgs.stdenv.hostPlatform.isLinux (
              lib.makeLibraryPath [ pkgs.libclang.lib ]
            );
            LD_LIBRARY_PATH = lib.optionalString pkgs.stdenv.hostPlatform.isLinux (
              lib.makeLibraryPath linuxLibraries
            );

            shellHook = lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              export PATH="${systemXcrun}/bin:$PATH"
              export DEVELOPER_DIR="''${DEVELOPER_DIR:-$(/usr/bin/xcode-select -p)}"
              export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
              export CC="$(xcrun --find clang)"
              export CXX="$(xcrun --find clang++)"
              export LIBCLANG_PATH="$DEVELOPER_DIR/Toolchains/XcodeDefault.xctoolchain/usr/lib"

              if ! xcrun metal --version >/dev/null 2>&1; then
                echo "prmarmot: the local Xcode Metal toolchain is unavailable." >&2
                echo "Install Xcode, launch it once, then run:" >&2
                echo "  xcodebuild -downloadComponent MetalToolchain" >&2
                return 1
              fi
            '';
          };
        }
      );

      checks = forAllSystems (
        system:
        let
          pkgs = pkgsFor.${system};
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
        in
        {
          inherit (self.packages.${system}) prmarmot;

          core = rustPlatform.buildRustPackage {
            pname = "prmarmot-core-check";
            version = "0.1.0";
            src = nixpkgs.lib.cleanSource self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "--package"
              "prmarmot-core"
            ];
            cargoTestFlags = [
              "--package"
              "prmarmot-core"
            ];
            installPhase = ''
              touch "$out"
            '';
          };
        }
      );

      formatter = forAllSystems (system: pkgsFor.${system}.nixfmt);
    };
}
