{
  # IMVUWU for NixOS (and Nix on any Linux).
  #   nix build            -> ./result/bin/imvuwu  (no embedded IMVU avatars;
  #                           the first-run screen downloads them)
  #   nix build .#full     -> with the developer's embedded avatars (only in
  #                           the developer's checkout; they are not published)
  #   nix run              -> build and start
  #   nix develop          -> shell with cargo, clippy and the runtime libraries
  # A plain Linux binary from build.sh does not run on NixOS as-is (no
  # /lib64/ld-linux.so or /usr/lib); this flake builds one that does.
  description = "IMVUWU: IMVU creator tools (HeadMaker, FaceMaker, Avatar Poser, Packages)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # winit, glutin and wgpu load these at run time (dlopen), so they go
      # on the wrapper's LD_LIBRARY_PATH rather than being linked.
      runtimeLibs = pkgs: with pkgs; [
        libGL
        libxkbcommon
        wayland
        vulkan-loader
        xorg.libX11
        xorg.libXcursor
        xorg.libXi
        xorg.libXrandr
      ];
      imvuwu = pkgs: { full ? false }:
        let cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        in pkgs.rustPlatform.buildRustPackage {
          pname = "imvuwu";
          version = cargoToml.package.version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          buildFeatures = nixpkgs.lib.optional full "bundled-imvu-avatars";
          nativeBuildInputs = [ pkgs.makeWrapper pkgs.pkg-config ];
          # The Rust tests use the embedded avatars; run them with build.sh.
          doCheck = false;
          postInstall = ''
            mv $out/bin/headmaker $out/bin/imvuwu
            wrapProgram $out/bin/imvuwu \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)}
            install -Dm644 assets/icon.png $out/share/icons/hicolor/128x128/apps/imvuwu.png
            mkdir -p $out/share/applications
            cat > $out/share/applications/imvuwu.desktop <<EOF
            [Desktop Entry]
            Type=Application
            Name=IMVUWU
            Comment=IMVU creator tools
            Exec=imvuwu
            Icon=imvuwu
            Categories=Graphics;3DGraphics;
            EOF
          '';
          meta.mainProgram = "imvuwu";
        };
    in {
      packages = forAll (pkgs: {
        default = imvuwu pkgs { };
        full = imvuwu pkgs { full = true; };
      });
      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc clippy rustfmt pkg-config ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}
