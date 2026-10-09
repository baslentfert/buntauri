# Build Bun on Windows without touching the Bun tree or global settings.
#
#   pwsh -File scripts\build-bun.ps1                 # debug build -> <bun>\build\debug\bun-debug.exe
#   pwsh -File scripts\build-bun.ps1 -Target build:release
#
# - Picks a Visual Studio that actually has MSVC. Bun's vs-shell.ps1 takes the
#   newest install, which may be a Preview without the C++ workload; it skips
#   detection when a VS environment is already loaded, so we load one first.
# - Points Bun at LLVM via BUN_TOOLCHAIN_LLVM, so the pinned LLVM can live
#   next to another system LLVM.
param(
    [string]$BunDir = (Join-Path $PSScriptRoot "..\..\bun"),
    [string]$Llvm = (Join-Path $env:USERPROFILE "scoop\apps\llvm\current"),
    [string]$Target = "build"
)
$ErrorActionPreference = "Stop"

# Fresh PATH from the registry, so tools installed since this shell started
# (e.g. via scoop) are found. vswhere's folder is added for Launch-VsDevShell.
$vsInstaller = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer"
$env:Path = @(
    [Environment]::GetEnvironmentVariable("Path", "Machine"),
    [Environment]::GetEnvironmentVariable("Path", "User"),
    $vsInstaller
) -join ";"

if (-not $env:VSINSTALLDIR) {
    $vswhere = Join-Path $vsInstaller "vswhere.exe"
    # MSVC + ATL (Bun's rescle.cpp, used for --windows-icon etc., needs atlstr.h).
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 Microsoft.VisualStudio.Component.VC.ATL -property installationPath
    if (-not $vs) { throw "No Visual Studio with MSVC and ATL found. Add 'C++ ATL' in the Visual Studio Installer." }
    Write-Host "Using Visual Studio: $vs"
    & (Join-Path $vs "Common7\Tools\Launch-VsDevShell.ps1") -Arch amd64 -HostArch amd64 -SkipAutomaticLocation | Out-Null
}

if (-not (Test-Path (Join-Path $Llvm "bin\clang.exe"))) { throw "LLVM not found at $Llvm" }
$env:BUN_TOOLCHAIN_LLVM = (Resolve-Path $Llvm).Path
Write-Host "Using LLVM: $env:BUN_TOOLCHAIN_LLVM"

Set-Location (Resolve-Path $BunDir)
. .\scripts\vs-shell.ps1
bun run $Target
exit $LASTEXITCODE
