Param(
  [string]$Prefix = "$HOME\.local\bin",
  [switch]$InstallRust,
  [switch]$SkipPsmux
)

$ErrorActionPreference = "Stop"

function Ensure-Command {
  param(
    [string]$Name,
    [string]$HelpMessage
  )
  if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
    throw $HelpMessage
  }
}

function Add-UserPathIfMissing {
  param([string]$Dir)

  $expanded = [System.IO.Path]::GetFullPath($Dir)
  $currentUserPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ([string]::IsNullOrWhiteSpace($currentUserPath)) {
    [Environment]::SetEnvironmentVariable("Path", $expanded, "User")
    return
  }

  $entries = $currentUserPath.Split(';') | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
  $normalized = $entries | ForEach-Object {
    try { [System.IO.Path]::GetFullPath($_) } catch { $_ }
  }

  if ($normalized -contains $expanded) {
    return
  }

  [Environment]::SetEnvironmentVariable("Path", "$currentUserPath;$expanded", "User")
}

if ($InstallRust) {
  if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
      throw "cargo not found and winget is unavailable. Install Rust from https://rustup.rs/ and rerun."
    }

    Write-Host "==> Installing Rust (rustup) via winget"
    winget install -e --id Rustlang.Rustup --accept-package-agreements --accept-source-agreements

    $cargoBin = Join-Path $HOME ".cargo\bin"
    if (Test-Path $cargoBin) {
      $env:Path = "$cargoBin;$env:Path"
    }
  }
}

Ensure-Command git "git is required. Install Git for Windows and rerun."
Ensure-Command cargo "cargo is required. Install Rust from https://rustup.rs/ or rerun with -InstallRust."

if (-not $SkipPsmux) {
  if (-not (Get-Command tmux -ErrorAction SilentlyContinue)) {
    Write-Host "==> tmux command not found. Installing psmux (Windows-native tmux alternative)."

    if (Get-Command winget -ErrorAction SilentlyContinue) {
      winget install -e --id marlocarlo.psmux --accept-package-agreements --accept-source-agreements
    }
    elseif (Get-Command cargo -ErrorAction SilentlyContinue) {
      cargo install psmux
    }
    else {
      throw "Could not install psmux automatically (no winget/cargo available)."
    }

    $cargoBin = Join-Path $HOME ".cargo\bin"
    if (Test-Path $cargoBin) {
      $env:Path = "$cargoBin;$env:Path"
    }
  }
}

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $repoRoot

Write-Host "==> Building release binaries"
cargo build --release

Write-Host "==> Installing binaries to $Prefix"
New-Item -ItemType Directory -Force -Path $Prefix | Out-Null

$srcDir = Join-Path $repoRoot "target\release"
$targets = @("br.exe", "b.exe", "cx.exe")
foreach ($exe in $targets) {
  $src = Join-Path $srcDir $exe
  if (-not (Test-Path $src)) {
    throw "Build did not produce $src"
  }
  Copy-Item -Force $src (Join-Path $Prefix $exe)
}

Add-UserPathIfMissing $Prefix

Write-Host "==> Installed"
Write-Host "  $(Join-Path $Prefix 'br.exe')"
Write-Host "  $(Join-Path $Prefix 'b.exe')"
Write-Host "  $(Join-Path $Prefix 'cx.exe')"
Write-Host ""
Write-Host "Open a NEW PowerShell window, then verify:"
Write-Host "  br --help"
Write-Host "  b --help"
Write-Host "  cx --help"
if (-not $SkipPsmux) {
  Write-Host "  tmux --help"
}
Write-Host ""
if ($SkipPsmux) {
  Write-Host "Note: tmux/psmux must be available on PATH to run br/b sessions."
}
