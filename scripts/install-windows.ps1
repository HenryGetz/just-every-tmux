Param(
  [string]$Prefix = "$HOME\.local\bin",
  [switch]$InstallRust,
  [switch]$SkipPsmux,
  [switch]$UseMsvcBuildTools,
  [string]$PortableMsvcRoot = "$HOME\.portable-msvc",
  [string]$PortableMsvcVs = "2022"
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

function Ensure-Rust {
  if (Get-Command cargo -ErrorAction SilentlyContinue) {
    return
  }

  if (-not $InstallRust) {
    throw "cargo is required. Install Rust from https://rustup.rs/ or rerun with -InstallRust."
  }

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

  Ensure-Command cargo "cargo still not found after rustup install. Open a new terminal and rerun."
}

function Ensure-PythonCommand {
  if (Get-Command py -ErrorAction SilentlyContinue) {
    return @{ exe = "py"; args = @("-3") }
  }
  if (Get-Command python -ErrorAction SilentlyContinue) {
    return @{ exe = "python"; args = @() }
  }

  if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw "Python is required for portable-msvc bootstrap and winget is unavailable. Install Python 3 and rerun."
  }

  Write-Host "==> Installing Python 3 via winget"
  Invoke-Checked {
    winget install -e --id Python.Python.3.12 --accept-package-agreements --accept-source-agreements
  } "Install Python via winget"

  if (Get-Command py -ErrorAction SilentlyContinue) {
    return @{ exe = "py"; args = @("-3") }
  }
  if (Get-Command python -ErrorAction SilentlyContinue) {
    return @{ exe = "python"; args = @() }
  }

  throw "Python installation completed but python launcher not found in PATH. Open a new terminal and rerun."
}

function Invoke-PythonScript {
  param(
    [hashtable]$Python,
    [string]$ScriptPath,
    [string[]]$Arguments
  )

  & $Python.exe @($Python.args) $ScriptPath @Arguments
  if ($LASTEXITCODE -ne 0) {
    throw "portable-msvc.py failed (exit code $LASTEXITCODE)."
  }
}

function Ensure-PortableMsvcToolchain {
  $root = [System.IO.Path]::GetFullPath($PortableMsvcRoot)
  $msvcDir = Join-Path $root "msvc"
  $setupBat = Join-Path $msvcDir "setup_x64.bat"

  if (Test-Path $setupBat) {
    return $setupBat
  }

  $python = Ensure-PythonCommand

  New-Item -ItemType Directory -Force -Path $root | Out-Null
  $scriptPath = Join-Path $root "portable-msvc.py"
  if (-not (Test-Path $scriptPath)) {
    $url = "https://gist.githubusercontent.com/mmozeiko/7f3162ec2988e81e56d5c4e22cde9977/raw/portable-msvc.py"
    Write-Host "==> Downloading portable-msvc.py"
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $scriptPath
  }

  Write-Host "==> Provisioning portable MSVC toolchain"
  Push-Location $root
  try {
    Invoke-PythonScript -Python $python -ScriptPath $scriptPath -Arguments @(
      "--accept-license",
      "--vs", $PortableMsvcVs,
      "--host", "x64",
      "--target", "x64"
    )
  }
  finally {
    Pop-Location
  }

  if (-not (Test-Path $setupBat)) {
    throw "portable MSVC setup script not found at $setupBat"
  }

  return $setupBat
}

function Import-BatchEnvironment {
  param([string]$BatchFile)

  $raw = cmd /c "`"$BatchFile`" >nul && set"
  if ($LASTEXITCODE -ne 0) {
    throw "Failed to import environment from $BatchFile"
  }

  foreach ($line in $raw) {
    if ($line -notmatch "^[^=]+=.*$") {
      continue
    }
    $idx = $line.IndexOf("=")
    if ($idx -lt 1) {
      continue
    }

    $name = $line.Substring(0, $idx)
    $value = $line.Substring($idx + 1)
    if ($name.StartsWith("=")) {
      continue
    }

    Set-Item -Path "Env:$name" -Value $value
  }
}

function Install-MsvcBuildTools {
  if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw "MSVC build tools requested, but winget is unavailable. Install Visual Studio Build Tools manually."
  }

  Write-Host "==> Installing Visual Studio C++ Build Tools + Windows SDK"
  $override = "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  Invoke-Checked {
    winget install -e --id Microsoft.VisualStudio.2022.BuildTools --accept-package-agreements --accept-source-agreements --override $override
  } "Install Visual Studio Build Tools"
}

function Build-Release {
  Write-Host "==> Building release binaries"
  Invoke-Checked { cargo build --release } "cargo build --release"
}

Ensure-Command git "git is required. Install Git for Windows and rerun."
Ensure-Rust

if (Get-Command rustup -ErrorAction SilentlyContinue) {
  try {
    Invoke-Checked { rustup default stable-x86_64-pc-windows-msvc } "rustup default msvc"
  }
  catch {
    Write-Host "warning: could not set default Rust toolchain to MSVC automatically; continuing."
  }
}

if (-not $SkipPsmux) {
  if (-not (Get-Command tmux -ErrorAction SilentlyContinue)) {
    Write-Host "==> tmux command not found. Installing psmux (Windows-native tmux alternative)."
    if (Get-Command winget -ErrorAction SilentlyContinue) {
      Invoke-Checked {
        winget install -e --id marlocarlo.psmux --accept-package-agreements --accept-source-agreements
      } "Install psmux via winget"
    }
    else {
      Invoke-Checked { cargo install psmux } "Install psmux via cargo"
    }
  }
}

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Set-Location $repoRoot

if ($UseMsvcBuildTools) {
  Install-MsvcBuildTools
}
else {
  $setupBat = Ensure-PortableMsvcToolchain
  Write-Host "==> Activating portable MSVC environment"
  Import-BatchEnvironment -BatchFile $setupBat
}

Build-Release

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
if ($UseMsvcBuildTools) {
  Write-Host "MSVC source: Visual Studio Build Tools"
}
else {
  Write-Host "MSVC source: portable-msvc.py ($PortableMsvcRoot)"
}
