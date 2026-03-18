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

function Ensure-EmbeddedPython {
  $root = [System.IO.Path]::GetFullPath($PortableMsvcRoot)
  $embedDir = Join-Path $root "python-embed"
  $pythonExe = Join-Path $embedDir "python.exe"
  if (Test-Path $pythonExe) {
    return $pythonExe
  }

  New-Item -ItemType Directory -Force -Path $embedDir | Out-Null
  $tmpZip = Join-Path $root "python-embed.zip"

  $versions = @("3.12.10", "3.12.9", "3.12.8")
  $downloaded = $false
  foreach ($version in $versions) {
    $url = "https://www.python.org/ftp/python/$version/python-$version-embed-amd64.zip"
    try {
      Write-Host "==> Downloading embedded Python $version"
      Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $tmpZip
      $downloaded = $true
      break
    }
    catch {
      Write-Host "warning: failed to download $url"
    }
  }

  if (-not $downloaded) {
    throw "Could not download embedded Python runtime from python.org."
  }

  if (Test-Path $embedDir) {
    Get-ChildItem -Path $embedDir -Force | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
  }
  Expand-Archive -Path $tmpZip -DestinationPath $embedDir -Force
  Remove-Item -Force $tmpZip -ErrorAction SilentlyContinue

  if (-not (Test-Path $pythonExe)) {
    throw "Embedded Python extraction completed but python.exe was not found at $pythonExe"
  }

  return $pythonExe
}

function Ensure-PythonCommand {
  function Resolve-Python {
    param(
      [string]$Exe,
      [string[]]$PrefixArgs
    )

    try {
      $output = & $Exe @PrefixArgs -c "import sys; print(sys.executable)"
      if ($LASTEXITCODE -ne 0) {
        return $null
      }

      $text = ($output | Out-String).Trim()
      if ([string]::IsNullOrWhiteSpace($text)) {
        return $null
      }

      if ($text -match "Python was not found") {
        return $null
      }

      return @{ exe = $Exe; args = $PrefixArgs }
    }
    catch {
      return $null
    }
  }

  $candidates = @(
    @{ exe = "py"; args = @("-3") },
    @{ exe = "python"; args = @() },
    @{ exe = "python3"; args = @() }
  )

  foreach ($candidate in $candidates) {
    $resolved = Resolve-Python -Exe $candidate.exe -PrefixArgs $candidate.args
    if ($null -ne $resolved) {
      return $resolved
    }
  }

  if (Get-Command winget -ErrorAction SilentlyContinue) {
    Write-Host "==> Installing Python 3 via winget"
    try {
      Invoke-Checked {
        winget install -e --id Python.Python.3.12 --scope user --accept-package-agreements --accept-source-agreements
      } "Install Python via winget"
    }
    catch {
      Write-Host "warning: winget Python install failed; falling back to embedded Python runtime."
    }
  }
  else {
    Write-Host "==> winget not available; falling back to embedded Python runtime"
  }

  $possiblePathAdds = @(
    (Join-Path $HOME "AppData\\Local\\Programs\\Python\\Launcher"),
    (Join-Path $HOME "AppData\\Local\\Programs\\Python\\Python312"),
    (Join-Path $HOME "AppData\\Local\\Programs\\Python\\Python313"),
    (Join-Path $HOME "AppData\\Local\\Microsoft\\WindowsApps")
  )
  foreach ($p in $possiblePathAdds) {
    if (Test-Path $p) {
      $env:Path = "$p;$env:Path"
    }
  }

  foreach ($candidate in $candidates) {
    $resolved = Resolve-Python -Exe $candidate.exe -PrefixArgs $candidate.args
    if ($null -ne $resolved) {
      return $resolved
    }
  }

  $directPy = Get-ChildItem -Path (Join-Path $HOME "AppData\\Local\\Programs\\Python") -Filter "python.exe" -Recurse -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending |
    Select-Object -First 1
  if ($null -ne $directPy) {
    $resolved = Resolve-Python -Exe $directPy.FullName -PrefixArgs @()
    if ($null -ne $resolved) {
      return $resolved
    }
  }

  $embeddedPython = Ensure-EmbeddedPython
  $embeddedResolved = Resolve-Python -Exe $embeddedPython -PrefixArgs @()
  if ($null -ne $embeddedResolved) {
    return $embeddedResolved
  }

  throw "Python bootstrap failed. Could not find a runnable Python interpreter."
}

function Invoke-PythonScript {
  param(
    [hashtable]$Python,
    [string]$ScriptPath,
    [string[]]$Arguments
  )

  # Stream script logs to console but do not leak them into function return values.
  & $Python.exe @($Python.args) $ScriptPath @Arguments 2>&1 | Out-Host
  if ($LASTEXITCODE -ne 0) {
    throw "portable-msvc.py failed (exit code $LASTEXITCODE)."
  }
}

function Ensure-PortableMsvcToolchain {
  $root = [System.IO.Path]::GetFullPath($PortableMsvcRoot)
  $msvcDir = Join-Path $root "msvc"
  $setupBat = Join-Path $msvcDir "setup_x64.bat"

  if (Test-Path $setupBat) {
    return [string]$setupBat
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

  return [string]$setupBat
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

function Write-CmdShim {
  param(
    [string]$PrefixDir,
    [string]$CommandName,
    [string]$TargetExe
  )

  $shimPath = Join-Path $PrefixDir ("{0}.cmd" -f $CommandName)
  $shim = "@echo off`r`n`"$TargetExe`" %*`r`n"
  Set-Content -Path $shimPath -Value $shim -Encoding ASCII
  return $shimPath
}

function Test-IsAntivirusBlock {
  param([System.Management.Automation.ErrorRecord]$ErrorRecord)

  # Locale-agnostic Win32 codes:
  # 225 = ERROR_VIRUS_INFECTED, 226 = ERROR_VIRUS_DELETED
  $malwareWin32Codes = @(225, 226)
  $exception = $ErrorRecord.Exception

  while ($null -ne $exception) {
    $win32Code = [int](([uint32]$exception.HResult) -band 0xFFFF)
    if ($malwareWin32Codes -contains $win32Code) {
      return $true
    }

    $nativeCodeProp = $exception.PSObject.Properties["NativeErrorCode"]
    if ($null -ne $nativeCodeProp) {
      $nativeCode = 0
      if ([int]::TryParse([string]$nativeCodeProp.Value, [ref]$nativeCode) -and ($malwareWin32Codes -contains $nativeCode)) {
        return $true
      }
    }

    $exception = $exception.InnerException
  }

  $msg = $ErrorRecord.Exception.Message
  if ($msg -match "virus" -or $msg -match "potentially unwanted software") {
    return $true
  }

  return $false
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
$installedPaths = @()
$usedCmdShimFallback = $false

foreach ($exe in $targets) {
  $src = Join-Path $srcDir $exe
  if (-not (Test-Path $src)) {
    throw "Build did not produce $src"
  }

  $dst = Join-Path $Prefix $exe
  try {
    $samePath = $false
    if (Test-Path $dst) {
      $samePath = ([System.IO.Path]::GetFullPath($src) -ieq [System.IO.Path]::GetFullPath($dst))
    }

    if ($samePath) {
      $installedPaths += $src
      continue
    }

    Copy-Item -Force $src $dst -ErrorAction Stop
    $installedPaths += $dst
  }
  catch {
    if (Test-IsAntivirusBlock -ErrorRecord $_) {
      Write-Host "warning: Defender blocked copying $exe to $Prefix; installing command shim instead."

      if (Test-Path $dst) {
        try {
          Remove-Item -Force $dst -ErrorAction Stop
        }
        catch {
          throw "Defender fallback failed: could not remove existing executable at $dst. Without removing .exe, Windows may ignore the .cmd shim. Resolve the file lock/policy issue and rerun."
        }

        if (Test-Path $dst) {
          throw "Defender fallback failed: existing executable still present at $dst after delete attempt. Without removing .exe, Windows may ignore the .cmd shim. Resolve the file lock/policy issue and rerun."
        }
      }

      if (-not (Test-Path -Path $src -PathType Leaf)) {
        throw "Defender fallback failed: source binary was removed at $src (likely quarantined). Rebuild or restore the binary and rerun."
      }

      $cmdName = [System.IO.Path]::GetFileNameWithoutExtension($exe)
      $shimPath = Write-CmdShim -PrefixDir $Prefix -CommandName $cmdName -TargetExe $src
      $installedPaths += $shimPath
      $usedCmdShimFallback = $true
      continue
    }

    throw
  }
}

Add-UserPathIfMissing $Prefix

Write-Host "==> Installed"
foreach ($p in $installedPaths) {
  Write-Host "  $p"
}
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

if ($usedCmdShimFallback) {
  Write-Host ""
  Write-Host "Note: Defender blocked direct .exe copy. Installed .cmd shims that run binaries from:"
  Write-Host "  $srcDir"
  Write-Host "If your org policy allows, add an exclusion for this repo or approve the binaries in Windows Security."
}
