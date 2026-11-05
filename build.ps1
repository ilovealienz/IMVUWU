# Builds IMVUWU with an open-source toolchain only: Rust's
# x86_64-pc-windows-gnullvm toolchain and llvm-mingw
# (clang, lld, mingw-w64). No Visual Studio or Windows SDK is needed.
#
#   .\build.ps1          release build: dist\IMVUWU-<version>-no-imvu-avatars.exe
#                        (to share; downloads the avatars on first launch) and,
#                        where the developer's IMVU avatar files exist,
#                        dist\IMVUWU-<version>.exe and dist\IMVUWU.exe with them,
#                        plus the Linux executables, cross-built with Zig:
#                        dist\IMVUWU-<version>-linux-x86_64[-no-imvu-avatars]
#   .\build.ps1 -NoLinux skip the Linux executables
#   .\build.ps1 -Test    tests and clippy, both feature sets
#
# Rust, llvm-mingw and Zig live in .tools\. Missing llvm-mingw or Zig is
# downloaded once from its official release and checked against the
# published SHA-256. The Linux executables need glibc 2.28 or newer.
param([switch]$Test, [switch]$NoLinux)
$ErrorActionPreference = 'Stop'
$project = $PSScriptRoot
$llvmVersion = '20261020'
$llvmSha256 = 'fd5462b7f5d93b2ff981fcd3f9c7e427e47f3203c64ea91c27bcc444eebd74cd'
$llvmName = "llvm-mingw-$llvmVersion-ucrt-x86_64"
$llvmDir = Join-Path $project ".tools\$llvmName"
$zigVersion = '0.16.0'
$zigSha256 = '68659eb5f1e4eb1437a722f1dd889c5a322c9954607f5edcf337bc3684a75a7e'
$zigName = "zig-x86_64-windows-$zigVersion"
$zigDir = Join-Path $project ".tools\$zigName"
$linux = -not $Test -and -not $NoLinux
# The developer's IMVU avatar copies are not in the public repository; the
# builds with them embedded are made only where they exist.
$avatars = Test-Path -LiteralPath (Join-Path $project 'assets\poser\female')

# Downloads a release zip into .tools\downloads, checks its SHA-256 and
# unpacks it into .tools.
function Get-Tool([string]$Name, [string]$Url, [string]$Sha256, [string]$Size) {
    $downloads = Join-Path $project '.tools\downloads'
    New-Item -ItemType Directory -Path $downloads -Force | Out-Null
    $zip = Join-Path $downloads "$Name.zip"
    if (-not (Test-Path -LiteralPath $zip)) {
        Write-Host "Downloading $Name.zip ($Size) from $Url"
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $Url -OutFile $zip -UserAgent 'IMVUWU-build'
    }
    $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
    if ($hash -ne $Sha256) {
        Remove-Item -LiteralPath $zip
        throw "$Name.zip has the wrong SHA-256 ($hash); deleted it, run again"
    }
    Expand-Archive -LiteralPath $zip -DestinationPath (Join-Path $project '.tools') -Force
}

# Rust: the project-local rustup in .tools if present, else the user's own
# rustup (a fresh clone or CI), where the toolchain is installed if needed.
if (Test-Path -LiteralPath (Join-Path $project '.tools\cargo\bin\rustup.exe')) {
    $env:CARGO_HOME = Join-Path $project '.tools\cargo'
    $env:RUSTUP_HOME = Join-Path $project '.tools\rustup'
} else {
    if (-not $env:CARGO_HOME) { $env:CARGO_HOME = Join-Path $HOME '.cargo' }
    if (-not (Test-Path -LiteralPath (Join-Path $env:CARGO_HOME 'bin\rustup.exe'))) {
        throw 'Rust is not installed: get rustup from https://rustup.rs, then run this again'
    }
}
$rustup = Join-Path $env:CARGO_HOME 'bin\rustup.exe'
& $rustup toolchain install stable-x86_64-pc-windows-gnullvm --profile minimal --component clippy,rustfmt --no-self-update
if ($LASTEXITCODE -ne 0) { throw 'Could not install the gnullvm Rust toolchain' }
if (-not (Test-Path -LiteralPath (Join-Path $llvmDir 'bin\clang.exe'))) {
    Get-Tool $llvmName "https://github.com/mstorsjo/llvm-mingw/releases/download/$llvmVersion/$llvmName.zip" $llvmSha256 'about 180 MB'
}
if ($linux -and -not (Test-Path -LiteralPath (Join-Path $zigDir 'zig.exe'))) {
    Get-Tool $zigName "https://ziglang.org/download/$zigVersion/$zigName.zip" $zigSha256 'about 95 MB'
}
# Only the open-source tools on PATH (plus Windows itself and Git).
$env:PATH = (Join-Path $env:CARGO_HOME 'bin') + ';' + (Join-Path $llvmDir 'bin') + ';' + $zigDir + ';' +
    (Join-Path $env:WINDIR 'System32') + ';' + $env:WINDIR + ';' + (Join-Path $env:ProgramFiles 'Git\cmd')
Remove-Item Env:LIB, Env:INCLUDE -ErrorAction SilentlyContinue
# The open-source Windows host toolchain (rust-toolchain.toml just says stable).
$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnullvm'

# Writes a .tar.gz holding one file as an executable (mode 0755), without
# needing tar: a ustar header, the data padded to 512 bytes, two empty
# blocks, all gzip-compressed with .NET.
function New-TarGz([string]$Source, [string]$Name, [string]$Destination) {
    $data = [IO.File]::ReadAllBytes($Source)
    $header = New-Object byte[] 512
    $put = {
        param([int]$Offset, [string]$Text)
        $bytes = [Text.Encoding]::ASCII.GetBytes($Text)
        [Array]::Copy($bytes, 0, $header, $Offset, $bytes.Length)
    }
    $octal = { param([long]$Value, [int]$Digits) [Convert]::ToString($Value, 8).PadLeft($Digits, '0') }
    & $put 0 $Name
    & $put 100 ((& $octal 0x1ED 7) + "`0")                       # mode 0755
    & $put 108 ((& $octal 0 7) + "`0")                           # uid
    & $put 116 ((& $octal 0 7) + "`0")                           # gid
    & $put 124 ((& $octal $data.Length 11) + "`0")               # size
    & $put 136 ((& $octal ([DateTimeOffset]::UtcNow.ToUnixTimeSeconds()) 11) + "`0")
    & $put 148 '        '                                        # checksum placeholder
    & $put 156 '0'                                               # regular file
    & $put 257 "ustar`0"
    & $put 263 '00'
    $sum = 0
    foreach ($b in $header) { $sum += $b }
    & $put 148 ((& $octal $sum 6) + "`0 ")
    $out = [IO.File]::Create($Destination)
    try {
        $gzip = New-Object IO.Compression.GZipStream($out, [IO.Compression.CompressionLevel]::Optimal)
        $gzip.Write($header, 0, 512)
        $gzip.Write($data, 0, $data.Length)
        $pad = (512 - ($data.Length % 512)) % 512
        $gzip.Write((New-Object byte[] ($pad + 1024)), 0, $pad + 1024)
        $gzip.Dispose()
    } finally { $out.Dispose() }
}

function Invoke-Cargo {
    & cargo @args
    if ($LASTEXITCODE -ne 0) { throw "cargo $($args -join ' ') failed" }
}

Push-Location $project
try {
    if ($Test) {
        Invoke-Cargo test --locked
        Invoke-Cargo clippy --locked --release --all-targets '--' -D warnings
        if ($avatars) {
            Invoke-Cargo test --locked --features bundled-imvu-avatars --target-dir target\release-avatars
            Invoke-Cargo clippy --locked --release --all-targets --features bundled-imvu-avatars --target-dir target\release-avatars '--' -D warnings
        }
    } else {
        Invoke-Cargo build --release --locked
        New-Item -ItemType Directory -Path (Join-Path $project 'dist') -Force | Out-Null
        $version = [regex]::Match((Get-Content -LiteralPath (Join-Path $project 'Cargo.toml') -Raw), '(?m)^version = "([^"]+)"').Groups[1].Value
        Copy-Item -LiteralPath (Join-Path $project 'target\release\headmaker.exe') -Destination (Join-Path $project "dist\IMVUWU-$version-no-imvu-avatars.exe")
        if ($avatars) {
            Invoke-Cargo build --release --locked --features bundled-imvu-avatars --target-dir target\release-avatars
            $full = Join-Path $project "dist\IMVUWU-$version.exe"
            Copy-Item -LiteralPath (Join-Path $project 'target\release-avatars\release\headmaker.exe') -Destination $full
            try {
                Copy-Item -LiteralPath $full -Destination (Join-Path $project 'dist\IMVUWU.exe')
            } catch {
                Write-Warning "IMVUWU.exe may still be running. Updated executable saved as $full"
            }
        }
        if ($linux) {
            # Linux x86_64 for glibc 2.28+, with Zig as the C compiler and
            # linker (cargo-zigbuild).
            $target = 'x86_64-unknown-linux-gnu'
            & $rustup target add $target
            if ($LASTEXITCODE -ne 0) { throw 'Could not add the Linux Rust target' }
            if (-not (Test-Path -LiteralPath (Join-Path $env:CARGO_HOME 'bin\cargo-zigbuild.exe'))) {
                Invoke-Cargo install cargo-zigbuild --locked --version 0.24.1
            }
            Invoke-Cargo zigbuild --release --locked --target "$target.2.28" --target-dir target\linux
            $linuxShare = Join-Path $project "dist\IMVUWU-$version-linux-x86_64-no-imvu-avatars"
            Copy-Item -LiteralPath (Join-Path $project "target\linux\$target\release\headmaker") -Destination $linuxShare
            if ($avatars) {
                Invoke-Cargo zigbuild --release --locked --target "$target.2.28" --features bundled-imvu-avatars --target-dir target\linux-avatars
                Copy-Item -LiteralPath (Join-Path $project "target\linux-avatars\$target\release\headmaker") -Destination (Join-Path $project "dist\IMVUWU-$version-linux-x86_64")
            }
            # A downloaded file loses its executable bit; a .tar.gz keeps it
            # (mode 0755), so the shared build runs straight after unpacking.
            New-TarGz -Source $linuxShare -Name 'IMVUWU' -Destination "$linuxShare.tar.gz"
        }
    }
} finally { Pop-Location }
