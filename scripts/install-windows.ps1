Param(
  [string]$Prefix = "$HOME\.local\bin",
  [switch]$InstallRust,
  [switch]$SkipPsmux
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

function Invoke-Checked {
  param(
    [scriptblock]$Command,
    [string]$Description
  )
  & $Command
  if ($LASTEXITCODE -ne 0) {
    throw "$Description failed (exit code $LASTEXITCODE)."
  }
}

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

function Install-MsvcBuildTools {
  if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw "MSVC toolchain appears incomplete and winget is unavailable. Install Visual Studio Build Tools (C++ workload) manually and rerun."
  }

  Write-Host "==> Installing Visual Studio C++ Build Tools + Windows SDK"
  $override = "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  winget install -e --id Microsoft.VisualStudio.2022.BuildTools --accept-package-agreements --accept-source-agreements --override $override
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to install Visual Studio Build Tools automatically. Please install it manually and rerun."
  }
}

function Build-ReleaseWithAutoFix {
  Write-Host "==> Building release binaries"
  $buildLog = New-TemporaryFile
  try {
    & cargo build --release 2>&1 | Tee-Object -FilePath $buildLog | Out-Host
    $exitCode = $LASTEXITCODE
    if ($exitCode -eq 0) {
      return
    }

    $text = Get-Content $buildLog -Raw
    $missingMsvc = ($text -match "LNK1104") -and ($text -match "msvcrt\.lib")
    if ($missingMsvc) {
      Write-Host "==> Detected missing MSVC runtime libs (msvcrt.lib). Attempting automatic fix..."
      Install-MsvcBuildTools
      if (Get-Command rustup -ErrorAction SilentlyContinue) {
        Invoke-Checked { rustup default stable-x86_64-pc-windows-msvc } "rustup default msvc"
      }
      Write-Host "==> Retrying cargo build"
      Invoke-Checked { cargo build --release } "cargo build"
      return
    }

    throw "cargo build failed. See output above for details."
  }
  finally {
    Remove-Item -Force $buildLog -ErrorAction SilentlyContinue
  }
}

if ($InstallRust) {
  if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
      throw "cargo not found and winget is unavailable. Install Rust from https://rustup.rs/ and rerun."
    }

    Write-Host "==> Installing Rust (rustup) via winget"
    Invoke-Checked {
      winget install -e --id Rustlang.Rustup --accept-package-agreements --accept-source-agreements
    } "Install Rust via winget"

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
      Invoke-Checked {
        winget install -e --id marlocarlo.psmux --accept-package-agreements --accept-source-agreements
      } "Install psmux via winget"
    }
    elseif (Get-Command cargo -ErrorAction SilentlyContinue) {
      Invoke-Checked { cargo install psmux } "Install psmux via cargo"
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

Build-ReleaseWithAutoFix

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
