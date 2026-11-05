# Tests IMVUWU like a brand-new install, inside Windows Sandbox: a clean,
# throwaway Windows with no Visual C++ runtime, no previous IMVUWU settings
# or avatars, and (by default) no GPU, like a PC whose graphics driver is
# not installed yet. Everything inside is deleted when the Sandbox closes.
#
#   powershell -ExecutionPolicy Bypass -File tools\new-install-sandbox.ps1
#   ... -Exe dist\IMVUWU-1.0.1.exe     test a specific exe
#   ... -Gpu                           give the Sandbox the real GPU
#
# The exe starts by itself on the Sandbox desktop. After two minutes the
# Sandbox copies the app's startup log and a list of its AppData folder to
# target\sandbox\results on this PC, so a run can be checked afterwards.
# Requires Windows Sandbox (Windows Pro/Enterprise): Settings > System >
# Optional features > More Windows features > "Windows Sandbox", then reboot.
param(
    [string]$Exe,
    [switch]$Gpu
)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$sandbox = Join-Path $env:WINDIR 'System32\WindowsSandbox.exe'
if (-not (Test-Path -LiteralPath $sandbox)) {
    Write-Host 'Windows Sandbox is not enabled on this PC.'
    Write-Host 'Enable it: Settings > System > Optional features > More Windows features >'
    Write-Host '"Windows Sandbox" (needs Windows Pro and virtualisation in the BIOS), then reboot.'
    exit 1
}
if (-not $Exe) {
    $Exe = Get-ChildItem -LiteralPath (Join-Path $project 'dist') -Filter 'IMVUWU-*-no-imvu-avatars.exe' |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
    if (-not $Exe) { throw 'No dist\IMVUWU-*-no-imvu-avatars.exe found; build one first.' }
}
$Exe = (Resolve-Path -LiteralPath $Exe).Path

# Host side: the app (read-only in the Sandbox) and a results folder.
$root = Join-Path $project 'target\sandbox'
$app = Join-Path $root 'app'
$results = Join-Path $root 'results'
foreach ($dir in $app, $results) {
    if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
    New-Item -ItemType Directory -Path $dir | Out-Null
}
Copy-Item -LiteralPath $Exe -Destination (Join-Path $app 'IMVUWU.exe')

# Runs inside the Sandbox at logon: start the app, wait, collect results.
$run = @'
@echo off
start "" "C:\IMVUWU\app\IMVUWU.exe"
timeout /t 120 /nobreak >nul
set "OUT=C:\IMVUWU\results"
if exist "%APPDATA%\IMVUWU\startup-log.txt" (
  copy /y "%APPDATA%\IMVUWU\startup-log.txt" "%OUT%\startup-log.txt" >nul
) else (
  echo No startup-log.txt: the app reported no start-up problem.> "%OUT%\startup-log.txt"
)
dir /s /b "%APPDATA%\IMVUWU" > "%OUT%\appdata-files.txt" 2>&1
tasklist /fi "imagename eq IMVUWU.exe" > "%OUT%\running.txt"
echo done> "%OUT%\finished.txt"
'@
[IO.File]::WriteAllText((Join-Path $app 'run.cmd'), $run.Replace("`n", "`r`n"))

$wsb = @"
<Configuration>
  <vGPU>$(if ($Gpu) { 'Enable' } else { 'Disable' })</vGPU>
  <Networking>Enable</Networking>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$app</HostFolder>
      <SandboxFolder>C:\IMVUWU\app</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$results</HostFolder>
      <SandboxFolder>C:\IMVUWU\results</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>C:\IMVUWU\app\run.cmd</Command>
  </LogonCommand>
</Configuration>
"@
$config = Join-Path $root 'new-install.wsb'
[IO.File]::WriteAllText($config, $wsb)
Write-Host "Starting Windows Sandbox with $(Split-Path -Leaf $Exe) (GPU: $(if ($Gpu) { 'on' } else { 'off' }))."
Write-Host "Results appear in $results after about two minutes."
Start-Process -FilePath $config
