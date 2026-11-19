# Voicey Windows build script. Runs ON WINDOWS (invoked from WSL through
# scripts/winbuild.sh, or directly in a Windows terminal) against a Windows-side
# staging copy of the repository.
#
# Probes the toolchain first: if a prerequisite is missing it prints the exact
# missing list and the install command, and exits without attempting a build.
#
# Outputs (copied into <StageDir>\dist):
#   - Voicey-portable-<version>.zip   (PRIMARY: portable folder; unzip anywhere, run Voicey.exe, no admin rights)
#   - Voicey_<version>_x64-setup.exe  (NSIS installer; the zip's payload source)
#   - voicey.exe                      (the raw release binary)
#   - smoke screenshot(s)             (only with -Smoke)
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 [-StageDir <path>] [-Smoke]

param(
    [string]$StageDir = "",
    [switch]$Smoke
)

$ErrorActionPreference = "Stop"

if (-not $StageDir) {
    $StageDir = Join-Path $env:TEMP "voicey-build"
}

# ---------------------------------------------------------------------------
# 1. Toolchain probe. Every missing item blocks the build; none is assumed.
# ---------------------------------------------------------------------------
$missing = New-Object System.Collections.Generic.List[string]

function Test-Command([string]$Name) {
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

# Bun (package manager; also runs vite build + the tauri CLI)
if (-not (Test-Command "bun")) {
    $missing.Add("bun (package manager) -- install: powershell -c `"irm bun.sh/install.ps1 | iex`" (or: npm install -g bun)")
}

# Rust (cargo/rustc, msvc host toolchain)
if (-not (Test-Command "cargo")) {
    $missing.Add("cargo/rustc (Rust, x86_64-pc-windows-msvc) -- install: winget install Rustlang.Rustup ; rustup default stable-x86_64-pc-windows-msvc")
}

# MSVC C++ build tools (link.exe) -- required by the Rust msvc target
$vswhere = "C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe"
$msvcOk = $false
if (Test-Path $vswhere) {
    $msvc = & $vswhere -products "*" -all -find "VC/Tools/MSVC/*/bin/Hostx64/x64/link.exe" 2>$null | Select-Object -First 1
    if ($msvc) { $msvcOk = $true }
}
if (-not $msvcOk) {
    $missing.Add("MSVC C++ Build Tools (link.exe) -- install: winget install Microsoft.VisualStudio.2022.BuildTools --override `"--add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --passive`"")
}

# CMake -- required by the transcribe-cpp build
if (-not (Test-Command "cmake")) {
    $missing.Add("cmake -- install: winget install Kitware.CMake")
}

# glslc -- required by ggml's vulkan-shaders-gen (ships with the Vulkan SDK)
$glslc = Get-Command "glslc" -ErrorAction SilentlyContinue
if (-not $glslc -and $env:VULKAN_SDK) {
    $glslcPath = Join-Path $env:VULKAN_SDK "Bin\glslc.exe"
    if (Test-Path $glslcPath) { $glslc = $true }
}
if (-not $glslc) {
    $missing.Add("glslc / Vulkan SDK (Vulkan backend shader compilation) -- install: winget install KhronosGroup.VulkanSDK (then open a new terminal)")
}

# WebView2 runtime -- the app's UI host
$webview2 = Test-Path "C:\Program Files (x86)\Microsoft\EdgeWebView\Application"
if (-not $webview2) {
    $reg = Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9FC7492}" -ErrorAction SilentlyContinue
    if ($reg -and $reg.pb) { $webview2 = $true }
}
if (-not $webview2) {
    $missing.Add("Microsoft Edge WebView2 Runtime -- install: winget install Microsoft.EdgeWebView2Runtime")
}

if ($missing.Count -gt 0) {
    Write-Output "VOICEY-BUILD: prerequisites missing - build NOT attempted"
    foreach ($m in $missing) {
        Write-Output "  MISSING: $m"
    }
    exit 2
}

Write-Output "VOICEY-BUILD: toolchain probe OK (bun, cargo/msvc, cmake, glslc, WebView2)"

# ---------------------------------------------------------------------------
# 2. Build.
# ---------------------------------------------------------------------------
if (-not (Test-Path (Join-Path $StageDir "package.json"))) {
    Write-Output "VOICEY-BUILD: no repository staged at $StageDir (expected a synced checkout)"
    exit 3
}

Set-Location $StageDir

Write-Output "VOICEY-BUILD: bun install"
bun install
if ($LASTEXITCODE -ne 0) { Write-Output "VOICEY-BUILD: bun install failed ($LASTEXITCODE)"; exit 4 }

Write-Output "VOICEY-BUILD: tauri build (nsis bundle)"
bun run tauri build --bundles nsis
if ($LASTEXITCODE -ne 0) { Write-Output "VOICEY-BUILD: tauri build failed ($LASTEXITCODE)"; exit 5 }

$version = (Get-Content "src-tauri\tauri.conf.json" | ConvertFrom-Json).version
$releaseExe = Join-Path $StageDir "src-tauri\target\release\voicey.exe"
$nsisExe = Get-ChildItem (Join-Path $StageDir "src-tauri\target\release\bundle\nsis") -Filter "*-setup.exe" |
    Select-Object -First 1
if (-not (Test-Path $releaseExe) -or -not $nsisExe) {
    Write-Output "VOICEY-BUILD: build outputs missing (exe or nsis bundle)"
    exit 6
}

# ---------------------------------------------------------------------------
# 3. Portable zip: silent portable install from the NSIS payload, then zip it.
# ---------------------------------------------------------------------------
$portableDir = Join-Path $StageDir "portable"
if (Test-Path $portableDir) { Remove-Item -Recurse -Force $portableDir }
Write-Output "VOICEY-BUILD: silent portable install -> $portableDir"
$proc = Start-Process -FilePath $nsisExe.FullName -ArgumentList @("/S", "/PORTABLE", "/D=$portableDir") -Wait -PassThru
if ($proc.ExitCode -ne 0 -or -not (Test-Path (Join-Path $portableDir "voicey.exe"))) {
    Write-Output "VOICEY-BUILD: portable install failed (exit $($proc.ExitCode))"
    exit 7
}

# The NSIS template still writes a legacy `portable` marker file; storage is
# unconditional now, so drop the inert marker from the shipped tree.
$marker = Join-Path $portableDir "portable"
if (Test-Path $marker) { Remove-Item -Force $marker }

$distDir = Join-Path $StageDir "dist"
New-Item -ItemType Directory -Force -Path $distDir | Out-Null
$zipPath = Join-Path $distDir "Voicey-portable-$version.zip"
if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
Compress-Archive -Path (Join-Path $portableDir "*") -DestinationPath $zipPath
Copy-Item -Force $nsisExe.FullName (Join-Path $distDir "Voicey_$($version)_x64-setup.exe")
Copy-Item -Force $releaseExe (Join-Path $distDir "voicey.exe")

Write-Output "VOICEY-BUILD: artifacts"
Write-Output "  PRIMARY portable zip: $zipPath"
Write-Output "  installer:               $(Join-Path $distDir "Voicey_$($version)_x64-setup.exe")"
Write-Output "  binary:                  $(Join-Path $distDir "voicey.exe")"

# ---------------------------------------------------------------------------
# 4. Optional smoke run: launch the portable app, reveal the window by
#    starting a second instance (single-instance protocol), screenshot, close.
# ---------------------------------------------------------------------------
if ($Smoke) {
    Add-Type -AssemblyName System.Drawing
    Add-Type -AssemblyName System.Windows.Forms
    $shotDir = Join-Path $distDir "smoke"
    New-Item -ItemType Directory -Force -Path $shotDir | Out-Null
    $app = Start-Process -FilePath (Join-Path $portableDir "voicey.exe") -PassThru
    Start-Sleep -Seconds 6
    Start-Process -FilePath (Join-Path $portableDir "voicey.exe") | Out-Null
    Start-Sleep -Seconds 4
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
    $shot = Join-Path $shotDir "smoke-main.png"
    $bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Output "VOICEY-BUILD: smoke screenshot: $shot"

    # Verify single-folder storage: Data/ must appear beside the exe with
    # the settings store inside it (first-run model download targets
    # Data/models via HF_HOME; a full download is not exercised here).
    $smokeData = Join-Path $portableDir "Data"
    $smokeStore = Join-Path $smokeData "settings_store.json"
    if (Test-Path $smokeStore) {
        Write-Output "VOICEY-BUILD: smoke storage OK: $smokeStore exists"
    } else {
        Write-Output "VOICEY-BUILD: smoke storage MISSING: $smokeStore not found (Data dir: $(Test-Path $smokeData))"
    }

    Stop-Process -Name "voicey" -Force -ErrorAction SilentlyContinue
    Wait-Process -Name "voicey" -Timeout 10 -ErrorAction SilentlyContinue
}

Write-Output "VOICEY-BUILD: done"
exit 0
