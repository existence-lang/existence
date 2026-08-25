<#
.SYNOPSIS
  existence installer for Windows — downloads the prebuilt release binary.

.DESCRIPTION
  irm https://raw.githubusercontent.com/existence-lang/existence/main/install.ps1 | iex

  Environment:
    EXISTENCE_VERSION      release tag to install (default: latest release)
    EXISTENCE_INSTALL_DIR  directory to install into (default: %LOCALAPPDATA%\existence\bin)
    EXISTENCE_REPO         GitHub repo (default: existence-lang/existence)
#>

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo       = if ($env:EXISTENCE_REPO) { $env:EXISTENCE_REPO } else { 'existence-lang/existence' }
$InstallDir = if ($env:EXISTENCE_INSTALL_DIR) { $env:EXISTENCE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'existence\bin' }
$Version    = $env:EXISTENCE_VERSION

function Say($msg) { Write-Host "existence-install: $msg" }

$arch = if ([Environment]::Is64BitOperatingSystem) { 'x86_64' } else { throw 'existence requires 64-bit Windows' }
if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
  Say 'ARM64 detected; installing the x86_64 build (runs under emulation)'
}
$target = "$arch-pc-windows-msvc"

$asset = "existence-$target.zip"
if (-not $Version) {
  # releases/latest/download needs no API call (no unauthenticated rate limit).
  $Version = 'latest'
  $url = "https://github.com/$Repo/releases/latest/download/$asset"
} else {
  if ($Version -notmatch '^v') { $Version = "v$Version" }
  $url = "https://github.com/$Repo/releases/download/$Version/$asset"
}
$tmp   = Join-Path ([IO.Path]::GetTempPath()) ("existence-" + [Guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
  Say "downloading $asset ($Version)"
  Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp $asset) -UseBasicParsing
  Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force

  New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
  foreach ($bin in 'existence.exe', 'xist.exe') {
    $src = Join-Path $tmp $bin
    if (Test-Path $src) { Copy-Item $src (Join-Path $InstallDir $bin) -Force }
  }

  $exe = Join-Path $InstallDir 'existence.exe'
  $installed = & $exe --version
  if ($LASTEXITCODE -ne 0) { throw "installed binary failed to run: $exe" }
  Say "installed $installed -> $InstallDir"

  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (($userPath -split ';') -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable('Path', "$InstallDir;$userPath", 'User')
    $env:Path = "$InstallDir;$env:Path"
    Say "added $InstallDir to your user PATH (open a new terminal to pick it up)"
  }
}
finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
