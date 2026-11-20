# Voicey Windows build script. Runs ON WINDOWS (invoked from WSL through
# scripts/winbuild.sh, or directly in a Windows terminal) against a Windows-side
# staging copy of the repository.
#
# Probes the toolchain first: if a prerequisite is missing it prints the exact
# missing list and the install command, and exits without attempting a build.
#
# Outputs (copied into <StageDir>\dist):
#   - Voicey-portable-<version>.zip   (PRIMARY: release build, portable folder; unzip anywhere,
#                                      run Voicey.exe, no admin rights. Capture-inert by
#                                      construction: the grounding-trace module is compiled
#                                      into inert stubs in release builds.)
#   - Voicey-dev-portable-<version>.zip (DEV: debug-profile build carrying the compiled-in
#                                      grounding-trace capability; unzips to its own
#                                      Voicey-dev-<version> folder and ships DEV-BUILD.txt.
#                                      Tray tooltip shows "(Dev)"; opens with a console window.)
#   - Voicey_<version>_x64-setup.exe  (NSIS installer; the release zip's payload source)
#   - voicey.exe / voicey-dev.exe     (the raw release / dev binaries)
#   - smoke screenshot(s)             (only with -Smoke; plus the compile-time trace check:
#                                      dev exe carries the trace module, release exe does not)
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

# libclang.dll -- required by llama-cpp-sys-2's bindgen step (the screen
# vision model's bindings). The pip `libclang` package is the lightest
# install (no admin rights); LLVM itself works too.
$libclang = "clang\native\libclang.dll"
$libclangUser = Join-Path $env:APPDATA "Python\Python314\site-packages\$libclang"
$libclangPipMissing = $false
if (-not (Test-Path $libclangUser)) {
    if (-not (Test-Path "C:\Program Files\LLVM\bin\libclang.dll")) {
        $missing.Add("libclang.dll (bindgen for the vision model) -- install: python -m pip install libclang  (or winget install LLVM.LLVM, then set LIBCLANG_PATH)")
        $libclangPipMissing = $true
    }
}

# ninja -- llama.cpp's cmake build must use the Ninja generator: the VS
# generator's parallel custom-build rules race on the vulkan-shaders-gen
# ExternalProject ("The system cannot find the batch label specified").
$ninjaDirs = @(
    "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja",
    "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja"
)
$ninjaOk = $false
foreach ($dir in $ninjaDirs) {
    if (Test-Path (Join-Path $dir "ninja.exe")) { $ninjaOk = $true; break }
}
if (-not $ninjaOk) {
    $missing.Add("ninja (llama.cpp's cmake generator) -- ships with VS Build Tools' CMake component; or: winget install Ninja-build.Ninja")
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

Write-Output "VOICEY-BUILD: toolchain probe OK (bun, cargo/msvc, cmake, glslc, WebView2, libclang, ninja)"

# ---------------------------------------------------------------------------
# 2. Build.
# ---------------------------------------------------------------------------
if (-not (Test-Path (Join-Path $StageDir "package.json"))) {
    Write-Output "VOICEY-BUILD: no repository staged at $StageDir (expected a synced checkout)"
    exit 3
}

Set-Location $StageDir

# The vision model's native build needs these; see the probe block above.
# Ninja applies to every cmake build in the process (cmake bootstraps the
# MSVC environment itself for Ninja, so transcribe-cpp builds fine too).
$env:CMAKE_GENERATOR = "Ninja"
if (Test-Path $libclangUser) {
    $env:LIBCLANG_PATH = Split-Path $libclangUser
} elseif (Test-Path "C:\Program Files\LLVM\bin\libclang.dll") {
    $env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
}
foreach ($dir in $ninjaDirs) {
    if (Test-Path (Join-Path $dir "ninja.exe")) { $env:PATH = "$dir;$env:PATH"; break }
}

# Build through a short target dir: the deep default path under the stage
# overflows MSVC's ~260-character limit (cl.exe PDB writes fail with a
# misleading C1041 "multiple CL.EXE" error, and the VS generator's batch
# labels break on long paths too). transcribe-libs staging and every
# artifact path below read this location.
$env:CARGO_TARGET_DIR = "C:\v3rel"
$targetRelease = Join-Path $env:CARGO_TARGET_DIR "release"

Write-Output "VOICEY-BUILD: bun install"
bun install
if ($LASTEXITCODE -ne 0) { Write-Output "VOICEY-BUILD: bun install failed ($LASTEXITCODE)"; exit 4 }

Write-Output "VOICEY-BUILD: tauri build (nsis bundle)"
bun run tauri build --bundles nsis
if ($LASTEXITCODE -ne 0) { Write-Output "VOICEY-BUILD: tauri build failed ($LASTEXITCODE)"; exit 5 }

$version = (Get-Content "src-tauri\tauri.conf.json" | ConvertFrom-Json).version
$releaseExe = Join-Path $targetRelease "voicey.exe"
$nsisExe = Get-ChildItem (Join-Path $targetRelease "bundle\nsis") -Filter "*-setup.exe" |
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

# Ship the license and third-party notices with the app (the NSIS payload
# only carries binaries and resources).
foreach ($doc in @("LICENSE", "NOTICE.md")) {
    $docPath = Join-Path $StageDir $doc
    if (Test-Path $docPath) {
        Copy-Item -Force $docPath (Join-Path $portableDir $doc)
    }
}

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
# 4. Dev build: a debug-profile Windows build with the compiled-in
#    grounding-trace capability (the D10 privacy rule keys on
#    debug_assertions, so only this flavor can ever write a trace). Packaged
#    as its own portable folder zip, visibly distinct from the release one:
#    its own zip name, a Voicey-dev top-level folder, and a DEV-BUILD.txt
#    marker. The app itself is distinguishable at a glance too: the tray
#    tooltip reads "Voicey v<version> (Dev)" and the debug exe opens with a
#    console window (windows_subsystem only in release builds).
# ---------------------------------------------------------------------------
Write-Output "VOICEY-BUILD: tauri build --debug (dev flavor)"
bun run tauri build --debug --bundles nsis
if ($LASTEXITCODE -ne 0) { Write-Output "VOICEY-BUILD: tauri build --debug failed ($LASTEXITCODE)"; exit 8 }

$targetDebug = Join-Path $env:CARGO_TARGET_DIR "debug"
$debugExe = Join-Path $targetDebug "voicey.exe"
$nsisDebugExe = Get-ChildItem (Join-Path $targetDebug "bundle\nsis") -Filter "*-setup.exe" |
    Select-Object -First 1
if (-not (Test-Path $debugExe) -or -not $nsisDebugExe) {
    Write-Output "VOICEY-BUILD: dev build outputs missing (exe or nsis bundle)"
    exit 9
}

$devFolderName = "Voicey-dev-$version"
$portableDevDir = Join-Path $StageDir $devFolderName
if (Test-Path $portableDevDir) { Remove-Item -Recurse -Force $portableDevDir }
Write-Output "VOICEY-BUILD: silent portable dev install -> $portableDevDir"
$proc = Start-Process -FilePath $nsisDebugExe.FullName -ArgumentList @("/S", "/PORTABLE", "/D=$portableDevDir") -Wait -PassThru
if ($proc.ExitCode -ne 0 -or -not (Test-Path (Join-Path $portableDevDir "voicey.exe"))) {
    Write-Output "VOICEY-BUILD: dev portable install failed (exit $($proc.ExitCode))"
    exit 10
}
$devMarker = Join-Path $portableDevDir "portable"
if (Test-Path $devMarker) { Remove-Item -Force $devMarker }
foreach ($doc in @("LICENSE", "NOTICE.md")) {
    $docPath = Join-Path $StageDir $doc
    if (Test-Path $docPath) {
        Copy-Item -Force $docPath (Join-Path $portableDevDir $doc)
    }
}

# A marker file so the dev flavor is unmistakable on disk, even unzipped.
$devReadme = @"
Voicey dev build (debug profile)
================================

This is NOT the production build. It is a debug build with the dev-only
grounding-trace capability compiled in.

The production Voicey-portable zip compiles the capture code out
entirely: a release build cannot write a trace, whatever its config
says.

Enabling the trace
-----------------
In this folder, run Voicey.exe once (it creates Data/), then edit
Data/config.toml and set:

    [dev]
    grounding_trace = true

Traces land under Data/logs/grounding/ - one session folder per grounded
dictation: screenshot.png and trace.json (the raw transcript, the exact
vision-model prompt, the model's extracted reading, and the final pasted
output).
"@
$devReadme | Set-Content -Path (Join-Path $portableDevDir "DEV-BUILD.txt") -Encoding ASCII

# The dev zip carries its own top-level folder (the release zip is flat),
# so the two flavors cannot be unzipped over each other by accident.
$devZipPath = Join-Path $distDir "Voicey-dev-portable-$version.zip"
if (Test-Path $devZipPath) { Remove-Item -Force $devZipPath }
Compress-Archive -Path $portableDevDir -DestinationPath $devZipPath
Copy-Item -Force $debugExe (Join-Path $distDir "voicey-dev.exe")

Write-Output "VOICEY-BUILD: artifacts (dev flavor)"
Write-Output "  DEV portable zip:        $devZipPath"
Write-Output "  dev binary:              $(Join-Path $distDir "voicey-dev.exe")"

# ---------------------------------------------------------------------------
# 5. Optional smoke run: launch each portable flavor, reveal the window by
#    starting a second instance (single-instance protocol), screenshot, close.
#    Also checks the compile-time privacy rule on the shipped binaries: the
#    dev exe must carry the trace module's string, the release exe must not.
# ---------------------------------------------------------------------------

# Streaming ASCII byte scan (needle is plain ASCII, so a non-ASCII byte
# maps to '?' harmlessly). Chunks with a needle-width carry so a string
# straddling a chunk boundary is still found, without loading a
# multi-hundred-MB debug exe into memory at once.
function Test-ExeContains([string]$ExePath, [string]$Needle) {
    $enc = [System.Text.Encoding]::ASCII
    $fs = [System.IO.File]::OpenRead($ExePath)
    try {
        $chunkSize = 4MB
        $buf = New-Object byte[] $chunkSize
        $carry = ""
        while (($read = $fs.Read($buf, 0, $chunkSize)) -gt 0) {
            $chunk = $enc.GetString($buf, 0, $read)
            if (($carry + $chunk).Contains($Needle)) { return $true }
            if ($chunk.Length -ge $Needle.Length) {
                $carry = $chunk.Substring($chunk.Length - $Needle.Length + 1)
            } else {
                $carry = $chunk
            }
        }
    } finally {
        $fs.Dispose()
    }
    return $false
}

if ($Smoke) {
    Add-Type -AssemblyName System.Drawing
    Add-Type -AssemblyName System.Windows.Forms
    $shotDir = Join-Path $distDir "smoke"
    New-Item -ItemType Directory -Force -Path $shotDir | Out-Null

    # Compile-time privacy rule, checked on the shipped binaries: the trace
    # module's log string ("Dev grounding trace") exists only in the imp
    # module, which release builds compile into inert stubs - so the dev exe
    # must carry it and the release exe must not.
    $releaseInert = -not (Test-ExeContains $releaseExe "Dev grounding trace")
    $devCapable = Test-ExeContains (Join-Path $portableDevDir "voicey.exe") "Dev grounding trace"
    if ($releaseInert -and $devCapable) {
        Write-Output "VOICEY-BUILD: smoke trace-module check OK: release exe inert, dev exe carries the compiled-in trace"
    } else {
        Write-Output "VOICEY-BUILD: smoke trace-module check FAILED (release inert: $releaseInert, dev compiled-in: $devCapable)"
    }

    $app = Start-Process -FilePath (Join-Path $portableDir "voicey.exe") -PassThru
    Start-Sleep -Seconds 6
    Start-Process -FilePath (Join-Path $portableDir "voicey.exe") | Out-Null
    Start-Sleep -Seconds 4
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    function Save-Screen([string]$Name) {
        $bmp = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
        $shot = Join-Path $shotDir $Name
        $bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
        $g.Dispose(); $bmp.Dispose()
        Write-Output "VOICEY-BUILD: smoke screenshot: $shot"
        return $shot
    }

    Save-Screen "smoke-main.png" | Out-Null

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

    # Dev flavor: same launch-reveal-screenshot-close round, on the debug
    # build (slower startup; the console window it opens shows up in the
    # screenshot as an extra flavor cue).
    Write-Output "VOICEY-BUILD: smoke run (dev flavor)"
    Start-Process -FilePath (Join-Path $portableDevDir "voicey.exe") | Out-Null
    Start-Sleep -Seconds 10
    Start-Process -FilePath (Join-Path $portableDevDir "voicey.exe") | Out-Null
    Start-Sleep -Seconds 4
    Save-Screen "smoke-dev-main.png" | Out-Null

    $smokeDevStore = Join-Path $portableDevDir "Data\settings_store.json"
    if (Test-Path $smokeDevStore) {
        Write-Output "VOICEY-BUILD: dev smoke storage OK: $smokeDevStore exists"
    } else {
        Write-Output "VOICEY-BUILD: dev smoke storage MISSING: $smokeDevStore not found (Data dir: $(Test-Path (Join-Path $portableDevDir "Data")))"
    }

    Stop-Process -Name "voicey" -Force -ErrorAction SilentlyContinue
    Wait-Process -Name "voicey" -Timeout 10 -ErrorAction SilentlyContinue
}

Write-Output "VOICEY-BUILD: done"
exit 0
