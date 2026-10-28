# Starts NetWeevil on Windows: builds what is missing (once), then opens the
# console in the browser. Data goes to %USERPROFILE%\NetWeevil unless
# NETWEEVIL_WORKSPACE is set. Double-click start.cmd, or run this in PowerShell.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Workspace = if ($env:NETWEEVIL_WORKSPACE) { $env:NETWEEVIL_WORKSPACE } else { Join-Path $env:USERPROFILE "NetWeevil" }
$Bin = Join-Path $Root "target\release\netweevil.exe"
$Bind = if ($env:NETWEEVIL_BIND) { $env:NETWEEVIL_BIND } else { "127.0.0.1:8080" }

$needConsole = -not (Test-Path (Join-Path $Root "frontend\dist\index.html"))
if (-not (Test-Path $Bin) -or $needConsole) {
  if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host ""
    Write-Host "NetWeevil needs to be built once, and the Rust toolchain is not installed."
    Write-Host "Install it from https://rustup.rs, then run this script again."
    Write-Host "Or download a prebuilt release archive and use its start script."
    Read-Host "Press Enter to close"
    exit 1
  }
  if ($needConsole) {
    if (Get-Command pnpm -ErrorAction SilentlyContinue) {
      Write-Host "==> Building the web console"
      Push-Location (Join-Path $Root "frontend"); pnpm install --frozen-lockfile; pnpm build; Pop-Location
    } elseif (Get-Command npm -ErrorAction SilentlyContinue) {
      Write-Host "==> Building the web console with npm"
      Push-Location (Join-Path $Root "frontend"); npm install; npm run build; Pop-Location
    } else {
      Write-Host "Node.js is not installed; the console cannot be built. Install it from https://nodejs.org."
      Write-Host "Continuing with the API only."
    }
  }
  Write-Host "==> Building NetWeevil (release, first time takes several minutes)"
  Push-Location $Root; cargo build --release -p netweevil-cli; Pop-Location
}

New-Item -ItemType Directory -Force -Path $Workspace | Out-Null
Set-Location $Workspace
Write-Host "==> Starting NetWeevil in $Workspace"
& $Bin bootstrap --bind $Bind
