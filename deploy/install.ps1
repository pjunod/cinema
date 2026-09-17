<#
.SYNOPSIS
  Install plurx as a Windows service in one command.

.DESCRIPTION
  From an elevated PowerShell in the repository root:

    powershell -ExecutionPolicy Bypass -File deploy\install.ps1

  or, with GNU make on PATH, `make install`. The script builds plurxd.exe
  from this checkout (or takes -Binary <plurxd.exe or the release zip>),
  copies it to the install directory with plurx.example.toml, writes the
  config file if there is none, registers the automatic LocalSystem service
  through plurxd's own `service install`, opens TCP 32400 and UDP 32414 in
  Windows Firewall, and then waits for /readyz to answer before it reports
  the version the server says it is. Run it again to upgrade; the service is
  stopped, the binary replaced, and the service started.

  -Uninstall stops and removes the service and the firewall rules. It keeps
  the install directory, the config, and the data directory.

.PARAMETER Binary
  A prebuilt plurxd.exe, or the plurxd-windows-x86_64.zip release archive.
  Without it the script runs `cargo build --locked --release -p plurxd`.

.PARAMETER InstallDir
  Where plurxd.exe lives. Default: C:\Program Files\plurx

.PARAMETER ConfigDir
  Where plurx.toml lives. Default: %ProgramData%\plurx

.PARAMETER DryRun
  Print every command instead of running it.
#>
[CmdletBinding()]
param(
  [string]$Binary = '',
  [string]$InstallDir = (Join-Path $env:ProgramFiles 'plurx'),
  [string]$ConfigDir = (Join-Path $env:ProgramData 'plurx'),
  [switch]$Uninstall,
  [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$serviceName = 'plurxd'
$httpRule = 'plurx HTTP'
$gdmRule = 'plurx GDM discovery'
$exe = Join-Path $InstallDir 'plurxd.exe'
$config = Join-Path $ConfigDir 'plurx.toml'

function Say([string]$Message) { Write-Host "install: $Message" }
function Fail([string]$Message) { Write-Error "install: $Message"; exit 1 }

# Run an external program, or print it under -DryRun. Arguments are printed
# quoted so a dry-run line can be pasted back into PowerShell. -IgnoreExit is
# for commands whose non-zero exit is a normal answer (netsh deleting a rule
# that is not there).
function Invoke-Step([string]$Command, [string[]]$Arguments, [switch]$IgnoreExit) {
  $shown = ($Arguments | ForEach-Object { if ($_ -match '\s') { "'$_'" } else { $_ } }) -join ' '
  if ($DryRun) { Write-Host "+ $Command $shown"; return }
  & $Command @Arguments
  if (-not $IgnoreExit -and $LASTEXITCODE -ne $null -and $LASTEXITCODE -ne 0) {
    Fail "$Command $shown exited $LASTEXITCODE"
  }
}

# Run a PowerShell step (cmdlets do not splat from a string array the way
# programs do), or print its description under -DryRun.
function Step([string]$Description, [scriptblock]$Action) {
  if ($DryRun) { Write-Host "+ $Description"; return }
  & $Action
}

function Test-Admin {
  $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
  (New-Object Security.Principal.WindowsPrincipal $identity).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not $DryRun -and -not (Test-Admin)) {
  Fail 'run this from an elevated PowerShell (the service and firewall steps need it)'
}

function Resolve-Binary {
  if ($Binary -ne '') {
    if (-not (Test-Path $Binary)) { Fail "no such file: $Binary" }
    if ($Binary -like '*.zip') {
      $stage = Join-Path ([IO.Path]::GetTempPath()) ("plurx-install-" + [Guid]::NewGuid().ToString('n'))
      Say "expanding $Binary"
      if (-not $DryRun) {
        Expand-Archive -Path $Binary -DestinationPath $stage -Force
        $found = Get-ChildItem -Path $stage -Recurse -Filter 'plurxd.exe' | Select-Object -First 1
        if (-not $found) { Fail "no plurxd.exe inside $Binary" }
        return $found.FullName
      }
      return (Join-Path $stage 'plurxd.exe')
    }
    return (Resolve-Path $Binary).Path
  }
  if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Fail 'cargo is not on PATH; install the Rust toolchain from rust-toolchain.toml, or pass -Binary <plurxd.exe or release zip>'
  }
  Say "building plurxd (release, --locked) in $root"
  Push-Location $root
  try { Invoke-Step 'cargo' @('build', '--locked', '--release', '-p', 'plurxd') }
  finally { Pop-Location }
  $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
  $built = Join-Path $targetDir 'release\plurxd.exe'
  if (-not $DryRun -and -not (Test-Path $built)) { Fail "build produced no $built" }
  return $built
}

# ffmpeg.exe beside plurxd.exe wins over PATH, and PLURX_FFMPEG wins over both.
# A missing ffmpeg is installed with winget rather than reported.
function Ensure-FFmpeg {
  if ($env:PLURX_FFMPEG -and $env:PLURX_FFPROBE) { return }
  if ((Test-Path (Join-Path $InstallDir 'ffmpeg.exe')) -and (Test-Path (Join-Path $InstallDir 'ffprobe.exe'))) { return }
  if ((Get-Command ffmpeg -ErrorAction SilentlyContinue) -and (Get-Command ffprobe -ErrorAction SilentlyContinue)) { return }
  Say 'ffmpeg/ffprobe not found; installing with winget (Gyan.FFmpeg)'
  if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    Fail 'no ffmpeg and no winget; put ffmpeg.exe and ffprobe.exe beside plurxd.exe (a jellyfin-ffmpeg build is best) or set PLURX_FFMPEG/PLURX_FFPROBE, then rerun'
  }
  Invoke-Step 'winget' @('install', '--id', 'Gyan.FFmpeg', '-e', '--accept-source-agreements', '--accept-package-agreements')
}

function Wait-Ready([int]$Port) {
  $url = "http://127.0.0.1:$Port"
  if ($DryRun) { Say "would wait for $url/readyz"; return }
  Say "waiting for $url/readyz"
  for ($i = 0; $i -lt 60; $i++) {
    try {
      Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 "$url/readyz" | Out-Null
      $server = try { (Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 "$url/api/v1/server").Content } catch { "$url answers /readyz" }
      Say "ready: $server"
      Say "open $url"
      return
    } catch { Start-Sleep -Seconds 1 }
  }
  Fail "server did not become ready within 60s; read: Get-EventLog -LogName Application -Newest 50, and the logs under $ConfigDir\data"
}

function Install-Plurx {
  $source = Resolve-Binary
  $existing = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
  if ($existing -and $existing.Status -ne 'Stopped') {
    Say 'plurxd is running; stopping it for the upgrade'
    Step "Stop-Service $serviceName" { Stop-Service -Name $serviceName }
  }
  Step "New-Item -ItemType Directory -Force '$InstallDir'" { New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null }
  Step "New-Item -ItemType Directory -Force '$ConfigDir'" { New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null }
  Step "Copy-Item '$source' '$exe'" { Copy-Item -Force -Path $source -Destination $exe }
  $example = Join-Path $root 'plurx.example.toml'
  Step "Copy-Item '$example' '$InstallDir'" { Copy-Item -Force -Path $example -Destination (Join-Path $InstallDir 'plurx.example.toml') }
  if (-not (Test-Path $config)) {
    Say "writing $config from plurx.example.toml (service mode keeps data under $ConfigDir\data)"
    Step "Copy-Item '$example' '$config'" { Copy-Item -Path $example -Destination $config }
  }
  Ensure-FFmpeg
  if ($existing) {
    # `service install` refuses an existing registration, so an upgrade is
    # stop, replace, start, exactly as deploy/README.md describes it.
    Step "Start-Service $serviceName" { Start-Service -Name $serviceName }
  } else {
    Invoke-Step $exe @('service', 'install', '--config', $config)
  }
  foreach ($rule in @(@($httpRule, 'TCP', '32400'), @($gdmRule, 'UDP', '32414'))) {
    Invoke-Step 'netsh' @('advfirewall', 'firewall', 'delete', 'rule', "name=$($rule[0])") -IgnoreExit
    Invoke-Step 'netsh' @('advfirewall', 'firewall', 'add', 'rule', "name=$($rule[0])", 'dir=in', 'action=allow', "protocol=$($rule[1])", "localport=$($rule[2])")
  }
  Wait-Ready 32400
  if (-not $DryRun) { Get-Service -Name $serviceName | Format-Table -AutoSize | Out-String | Write-Host }
  Say "config: $config"
}

function Uninstall-Plurx {
  if (Get-Service -Name $serviceName -ErrorAction SilentlyContinue) {
    if (Test-Path $exe) {
      Invoke-Step $exe @('service', 'uninstall')
    } else {
      # The binary is gone but the registration is not: let the SCM drop it.
      Step "Stop-Service $serviceName" { Stop-Service -Name $serviceName -ErrorAction SilentlyContinue }
      Invoke-Step 'sc.exe' @('delete', $serviceName)
    }
  } else {
    Say 'no plurxd service is registered'
  }
  foreach ($name in @($httpRule, $gdmRule)) {
    Invoke-Step 'netsh' @('advfirewall', 'firewall', 'delete', 'rule', "name=$name") -IgnoreExit
  }
  Say "removed the service and firewall rules; kept $InstallDir and $ConfigDir"
}

if ($Uninstall) { Uninstall-Plurx } else { Install-Plurx }
