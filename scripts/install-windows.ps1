Param(
  [string]$Distro = "Ubuntu",
  [string]$RepoUrl = "https://github.com/HenryGetz/just-every-tmux.git",
  [string]$RepoDir = "~/just-every-tmux"
)

$ErrorActionPreference = "Stop"

function Require-Command {
  param([string]$Name)
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw "Required command not found: $Name"
  }
}

Require-Command wsl

Write-Host "==> Checking WSL distribution '$Distro'"
$distros = wsl -l -q
if (-not ($distros -match "^$Distro$")) {
  Write-Host "WSL distro '$Distro' is not installed."
  Write-Host "Run this first (PowerShell as Admin), then reboot if prompted:"
  Write-Host "  wsl --install -d $Distro"
  exit 1
}

$wslScript = @"
set -euo pipefail

if ! command -v git >/dev/null 2>&1; then
  sudo apt update
  sudo apt install -y git
fi

if ! command -v tmux >/dev/null 2>&1; then
  sudo apt update
  sudo apt install -y tmux curl build-essential pkg-config libssl-dev
fi

if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi

source "`$HOME/.cargo/env" || true

if [ -d "$RepoDir/.git" ]; then
  git -C "$RepoDir" pull --ff-only
else
  git clone "$RepoUrl" "$RepoDir"
fi

cd "$RepoDir"
./scripts/install.sh

echo
echo "Installed in WSL at: `\$HOME/.local/bin"
echo "Use these in WSL terminal: br, b, cx"
"@

Write-Host "==> Installing in WSL ($Distro)"
wsl -d $Distro -- bash -lc $wslScript

Write-Host "==> Success"

