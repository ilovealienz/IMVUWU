# IMVUWU

Creator tools for IMVU, in one native app for Windows, Linux and macOS:

- **Packages:** look up an IMVU product by ID or shop link. Browse its files and revisions, preview images and 3D meshes, and download everything (CHKN, ZIP or images with fixed names). Meshes export to OBJ with a one-click `Make blend` launcher for Blender.
- **HeadMaker:** edit XMF heads (morphs, symmetry, neck repair).
- **FaceMaker:** make XPF facial expressions.
- **Avatar Poser:** pose one or two avatars and export XAF.

IMVUWU is a fan-made tool. It is not made by or affiliated with IMVU. It uses only IMVU's public product data and never asks for a login.

## Download

Get the latest build from the **Releases** page:

| System | File | Run it |
|---|---|---|
| Windows 10/11 | `IMVUWU-<version>-windows-x86_64.exe` | Double-click. Windows SmartScreen may warn about an unsigned download: choose *More info › Run anyway*. |
| Linux (x86_64, glibc 2.28+) | `IMVUWU-<version>-linux-x86_64.tar.gz` | Unpack it, then run `./IMVUWU`. |
| macOS (Apple Silicon) | `IMVUWU-<version>-macos-arm64.tar.gz` | Unpack it and open `IMVUWU`. It's unsigned, so macOS blocks it the first time: allow it in *System Settings › Privacy & Security › Open Anyway*, or run `xattr -d com.apple.quarantine IMVUWU` once in Terminal. |

Nothing else needs installing. On first launch IMVUWU downloads IMVU's base avatars (female product 80 with its default skin, and male product 191) from IMVU's public servers. Settings and the downloaded avatars are stored in:
- `%APPDATA%\IMVUWU` on Windows
- `~/.local/share/IMVUWU` on Linux
- `~/Library/Application Support/IMVUWU` on macOS

**Graphics:** the 3D views use the GPU through wgpu (DirectX 12, Vulkan, Metal or OpenGL). They also run on a PC without a graphics driver, using Windows' software renderer, with OpenGL as a fallback.

**Linux:** the binary uses the desktop's own OpenGL/Vulkan, Wayland/X11 and file-picker service. On NixOS, use the flake (below).

## Build

All three need [Rust](https://rustup.rs).

- **Windows:** `.\build.ps1`. It uses only open-source tools: Rust's `gnullvm` toolchain and llvm-mingw, which it downloads and checksums itself. No Visual Studio is needed. It also cross-builds the Linux executable with Zig; `-NoLinux` skips that. `.\build.ps1 -Test` runs the tests and clippy.
- **Linux / macOS:** `./build.sh`, or `./build.sh test` for the tests.
- **NixOS:** `nix build`, then `./result/bin/imvuwu`. `nix develop` gives a development shell.

GitHub Actions builds and tests all three systems on every push. It also runs each download's built-in render checks on its own system. A `v*` tag publishes a release with the three downloads.

**IMVU files are not included.** IMVU's own files are not part of this repository. The build that embeds the developer's copies of the base avatars (feature `bundled-imvu-avatars`) and a few tests that need IMVU head meshes only work in the developer's own checkout. Everything else builds and tests here.

### Diagnostics

| Command | What it does |
|---|---|
| `--software-gpu` | Runs on the software graphics adapter, to test a PC without a graphics driver. |
| `--wgpu-render-check <folder> [--software]` | Renders the 3D check scenes without a window. |
| `--renderer wgpu\|glow\|glow-basic` | Picks the renderer. |
| `--imvu-check <report>` | Live check of the IMVU lookups. |

If the app can't start, it shows why and writes `startup-log.txt` in its data folder.

## Licence

MIT. See [LICENSE](LICENSE). IMVU and its content belong to IMVU; none of it is included here.
