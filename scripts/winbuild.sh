#!/usr/bin/env bash
# Voicey Windows build bridge (WSL side).
#
# Builds the real Windows artifact from this WSL checkout by staging the tree
# to a Windows directory (robocopy), then running scripts/build-windows.ps1 on
# the Windows side through one deliberate interop call. Never imports or
# restores the Windows PATH into WSL: every Windows binary is invoked by its
# explicit path.
#
# Usage: scripts/winbuild.sh [--stage <windows-dir>] [--smoke]
#
# Outputs: copied to /home/marcin/firstmate/data/voicey-phase0/artifacts (and
# left in the Windows stage's dist dir); the exact paths are printed.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DISTRO="${WSL_DISTRO_NAME:-Ubuntu}"
STAGE=""
SMOKE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --stage) STAGE="$2"; shift 2 ;;
    --smoke) SMOKE=1; shift ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "winbuild.sh: unknown argument: $1" >&2; exit 2 ;;
  esac
done

CMD_EXE=/mnt/c/Windows/System32/cmd.exe
ROBOCOPY=/mnt/c/Windows/System32/Robocopy.exe
POWERSHELL=/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe

for exe in "$CMD_EXE" "$ROBOCOPY" "$POWERSHELL"; do
  if [ ! -x "$exe" ]; then
    echo "winbuild.sh: WSL interop unavailable: $exe not found" >&2
    exit 3
  fi
done

if [ -z "$STAGE" ]; then
  # Default stage: the Windows user's temp dir (queried through interop).
  WIN_TEMP="$("$CMD_EXE" /c "echo %TEMP%" 2>/dev/null | tr -d '\r' | grep -m1 '^C:\\' || true)"
  if [ -z "$WIN_TEMP" ]; then
    echo "winbuild.sh: could not resolve Windows %TEMP%" >&2
    exit 3
  fi
  STAGE="${WIN_TEMP}\\voicey-build"
fi

WUNC='\\wsl.localhost\'"$DISTRO""${REPO_ROOT//\//\\}"
echo "== Staging checkout -> $STAGE (robocopy, excluding build outputs)"

# Robocopy exit codes 0-7 are success (1 = files copied). Paths contain no
# spaces, so the command needs no quoting (WSL interop mangles embedded
# quotes passed through cmd /c). /XD takes bare dir NAMES (a path form does
# not match the UNC source), and bare names also protect the stage's own
# build outputs from the /MIR delete pass — keeping src-tauri/target lets
# cargo rebuild incrementally between runs.
set +e
"$CMD_EXE" /c "robocopy $WUNC $STAGE /MIR /NFL /NDL /NJH /NP /xd node_modules dist .git target"
rc=$?
set -e
if [ "$rc" -ge 8 ]; then
  echo "winbuild.sh: robocopy failed (exit $rc)" >&2
  exit 4
fi

PS_ARGS=(-NoProfile -ExecutionPolicy Bypass -File "${STAGE}\\scripts\\build-windows.ps1" -StageDir "$STAGE")
if [ "$SMOKE" = 1 ]; then
  PS_ARGS+=(-Smoke)
fi

echo "== Running Windows build in $STAGE"
set +e
"$POWERSHELL" "${PS_ARGS[@]}"
build_rc=$?
set -e

echo "== Windows build exited with $build_rc"

# Copy artifacts back into WSL, whatever the exit code (a missing-toolchain
# report still gets mirrored for the record).
win_part="${STAGE#*:}"        # "\Users\...\voicey-build"
stage_wsl="/mnt/c${win_part//\\//}"
dist_wsl="$stage_wsl/dist"
if [ -d "$dist_wsl" ]; then
  out_dir="/home/marcin/firstmate/data/voicey-phase0/artifacts"
  mkdir -p "$out_dir"
  cp -v "$dist_wsl"/* "$out_dir"/ 2>/dev/null || true
  echo "== Artifacts copied to $out_dir"
  ls -la "$out_dir"
fi

echo "(Windows stage left at $STAGE for inspection; remove it manually if unwanted)"
exit "$build_rc"
