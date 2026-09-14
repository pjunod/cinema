[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Plurxd,
    [Parameter(Mandatory = $true)]
    [string]$Ffmpeg,
    [Parameter(Mandatory = $true)]
    [string]$Ffprobe,
    [int]$Port = 32491,
    [string]$WorkRoot = (Join-Path $env:TEMP "plurx-windows-smoke")
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Invoke-PlurxJson {
    param(
        [Parameter(Mandatory = $true)][string]$Method,
        [Parameter(Mandatory = $true)][string]$Uri,
        [object]$Body,
        [string]$Token
    )
    $headers = @{}
    if ($Token) { $headers.Authorization = "Bearer $Token" }
    $parameters = @{
        Method = $Method
        Uri = $Uri
        Headers = $headers
        TimeoutSec = 30
    }
    if ($null -ne $Body) {
        $parameters.ContentType = "application/json"
        $parameters.Body = ($Body | ConvertTo-Json -Depth 12 -Compress)
    }
    Invoke-RestMethod @parameters
}

function Wait-ForLog {
    param([string]$Pattern, [int]$Seconds = 20)
    $deadline = (Get-Date).AddSeconds($Seconds)
    do {
        $text = @()
        if (Test-Path $script:Stdout) { $text += Get-Content $script:Stdout -Raw }
        if (Test-Path $script:Stderr) { $text += Get-Content $script:Stderr -Raw }
        if (($text -join "`n") -match $Pattern) { return }
        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)
    throw "timed out waiting for log pattern: $Pattern"
}

if (-not (Test-Path $Plurxd -PathType Leaf)) { throw "plurxd not found: $Plurxd" }
if (-not (Test-Path $Ffmpeg -PathType Leaf)) { throw "ffmpeg not found: $Ffmpeg" }
if (-not (Test-Path $Ffprobe -PathType Leaf)) { throw "ffprobe not found: $Ffprobe" }

$Plurxd = (Resolve-Path $Plurxd).Path
$Ffmpeg = (Resolve-Path $Ffmpeg).Path
$Ffprobe = (Resolve-Path $Ffprobe).Path
$Base = "http://127.0.0.1:$Port"
$RunRoot = Join-Path ([IO.Path]::GetFullPath($WorkRoot)) ("run-" + [guid]::NewGuid().ToString("N"))
$Data = Join-Path $RunRoot "data"
$Cache = Join-Path $RunRoot "cache"
$Scratch = Join-Path $RunRoot "scratch"
$Media = Join-Path $RunRoot "media"
$Config = Join-Path $RunRoot "plurx.toml"
$script:Stdout = Join-Path $RunRoot "plurxd.stdout.log"
$script:Stderr = Join-Path $RunRoot "plurxd.stderr.log"
$fixture = Join-Path $Media "Windows smoke.mkv"

New-Item -ItemType Directory -Force -Path $RunRoot | Out-Null
New-Item -ItemType Directory -Force -Path $Data, $Cache, $Scratch, $Media | Out-Null

& $Ffmpeg -hide_banner -loglevel error -f lavfi -i "testsrc2=size=640x360:rate=30" `
    -f lavfi -i "sine=frequency=440:sample_rate=48000" -t 300 -shortest `
    -c:v libx264 -preset ultrafast -g 60 -c:a aac -y $fixture
if ($LASTEXITCODE -ne 0) { throw "fixture creation failed with exit $LASTEXITCODE" }

@"
[server]
name = "Windows smoke"
bind = "127.0.0.1:$Port"

[storage]
data_dir = '$Data'
cache_dir = '$Cache'
transcode_dir = '$Scratch'
"@ | Set-Content -Path $Config -Encoding utf8

$baselineMediaPids = @(Get-Process -Name ffmpeg, ffprobe -ErrorAction SilentlyContinue | ForEach-Object Id)
$env:PLURX_FFMPEG = $Ffmpeg
$env:PLURX_FFPROBE = $Ffprobe
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public static class PlurxSmokeProcess {
    const uint GENERIC_WRITE = 0x40000000, FILE_SHARE_READ = 1, FILE_SHARE_WRITE = 2;
    const uint CREATE_ALWAYS = 2, FILE_ATTRIBUTE_NORMAL = 0x80;
    const uint STARTF_USESTDHANDLES = 0x100, CREATE_NEW_PROCESS_GROUP = 0x200;
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    struct STARTUPINFO { public uint cb; public string reserved, desktop, title; public uint x,y,xSize,ySize,xChars,yChars,fill,flags; public short show; public short reserved2; public IntPtr reservedPtr,input,output,error; }
    [StructLayout(LayoutKind.Sequential)]
    struct PROCESS_INFORMATION { public IntPtr process, thread; public uint processId, threadId; }
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr CreateFile(string name,uint access,uint share,IntPtr security,uint creation,uint flags,IntPtr template);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetHandleInformation(IntPtr handle,uint mask,uint flags);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool CreateProcess(string app,StringBuilder command,IntPtr pa,IntPtr ta,bool inherit,uint flags,IntPtr environment,string cwd,ref STARTUPINFO startup,out PROCESS_INFORMATION info);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool GenerateConsoleCtrlEvent(uint controlEvent,uint processGroupId);
    static string Quote(string value) { return "\"" + value.Replace("\"", "\\\"") + "\""; }
    public static int Start(string exe,string config,string stdout,string stderr) {
        IntPtr output=CreateFile(stdout,GENERIC_WRITE,FILE_SHARE_READ|FILE_SHARE_WRITE,IntPtr.Zero,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,IntPtr.Zero);
        IntPtr error=CreateFile(stderr,GENERIC_WRITE,FILE_SHARE_READ|FILE_SHARE_WRITE,IntPtr.Zero,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,IntPtr.Zero);
        if(output.ToInt64()==-1 || error.ToInt64()==-1) throw new Win32Exception();
        if(!SetHandleInformation(output,1,1) || !SetHandleInformation(error,1,1)) throw new Win32Exception();
        var startup=new STARTUPINFO(); startup.cb=(uint)Marshal.SizeOf(startup); startup.flags=STARTF_USESTDHANDLES; startup.output=output; startup.error=error; startup.input=IntPtr.Zero;
        var command=new StringBuilder(Quote(exe)+" --config "+Quote(config)+" run"); PROCESS_INFORMATION info;
        bool ok=CreateProcess(exe,command,IntPtr.Zero,IntPtr.Zero,true,CREATE_NEW_PROCESS_GROUP,IntPtr.Zero,null,ref startup,out info);
        int last=Marshal.GetLastWin32Error(); CloseHandle(output); CloseHandle(error);
        if(!ok) throw new Win32Exception(last); CloseHandle(info.thread); CloseHandle(info.process); return (int)info.processId;
    }
}
'@
$serverPid = [PlurxSmokeProcess]::Start($Plurxd, $Config, $script:Stdout, $script:Stderr)
$server = Get-Process -Id $serverPid

try {
    $deadline = (Get-Date).AddSeconds(90)
    do {
        if ($server.HasExited) { throw "plurxd exited during startup with $($server.ExitCode)" }
        try {
            $ready = Invoke-WebRequest -UseBasicParsing -Uri "$Base/readyz" -TimeoutSec 2
            if ($ready.StatusCode -eq 200) { break }
        } catch {}
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    if ((Get-Date) -ge $deadline) { throw "plurxd did not become ready" }

    $login = Invoke-PlurxJson POST "$Base/api/v1/setup" @{
        username = "windows-smoke"
        password = "windows-smoke-password"
    }
    $token = $login.token
    if (-not $token) { throw "setup did not return an admin token" }

    $library = Invoke-PlurxJson POST "$Base/api/v1/libraries" @{
        name = "Windows smoke"
        kind = "movies"
        paths = @($Media)
    } $token

    $deadline = (Get-Date).AddSeconds(120)
    do {
        $page = Invoke-PlurxJson GET "$Base/api/v1/libraries/$($library.id)/items" $null $token
        if ($page.items.Count -gt 0) { break }
        Start-Sleep -Seconds 1
    } while ((Get-Date) -lt $deadline)
    if ($page.items.Count -eq 0) { throw "fixture scan did not produce a library item" }

    $detail = Invoke-PlurxJson GET "$Base/api/v1/items/$($page.items[0].id)" $null $token
    $fileId = $detail.files[0].id
    if (-not $fileId) { throw "scanned item did not expose a media file" }

    $direct = Invoke-WebRequest -UseBasicParsing -Uri "$Base/api/v1/files/$fileId/direct" `
        -Headers @{ Authorization = "Bearer $token"; Range = "bytes=0-1023" } -TimeoutSec 30
    if ($direct.StatusCode -ne 206 -or $direct.RawContentLength -ne 1024) {
        throw "direct range smoke expected 206/1024 bytes, got $($direct.StatusCode)/$($direct.RawContentLength)"
    }

    $session = Invoke-PlurxJson POST "$Base/api/v1/files/$fileId/hls/sessions" @{
        playback_id = "windows-native-smoke"
        request_id = "windows-native-smoke-1"
        height = 360
        quality_auto = $false
        copy = $false
        start = 0
    } $token
    if (-not $session.session_id -or -not $session.playlist_url) {
        throw "HLS session response was incomplete"
    }

    $playlistUri = if ($session.playlist_url -match '^https?://') {
        $session.playlist_url
    } else {
        "$Base$($session.playlist_url)"
    }
    $deadline = (Get-Date).AddSeconds(120)
    do {
        try {
            $playlist = Invoke-WebRequest -UseBasicParsing -Uri $playlistUri `
                -Headers @{ Authorization = "Bearer $token" } -TimeoutSec 5
            if ($playlist.StatusCode -eq 200 -and $playlist.Content -match '#EXTM3U') { break }
        } catch {}
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    if (-not $playlist -or $playlist.Content -notmatch '#EXTM3U') {
        throw "HLS playlist did not become readable"
    }

    if ($session.control) {
        $clientId = [guid]::NewGuid().ToString()
        $selection = @{
            quality = @{ mode = "manual"; height = 360 }
            audio_track = $null
            subtitle = @{ mode = "off"; track = $null }
            audio_offset_ms = 0
            codec = "h264"
            dynamic_range = "sdr"
        }
        $capabilities = @{
            platform = "web"
            max_height = 360
            codecs = @("h264")
            dynamic_ranges = @("sdr")
            dual_player_preparation = $false
        }
        $controlUri = "$Base$($session.control.url)"
        Invoke-PlurxJson POST $controlUri @{
            protocol = $session.control.protocol
            generation = $session.control.generation
            control_epoch = $session.control.control_epoch
            client_instance_id = $clientId
            sequence = 1
            demand = "hold"
            position_ms = 0
            buffered_from_ms = 0
            buffered_through_ms = 0
            playback_rate = 0
            render_state = "waiting"
            seek_target_ms = $null
            observed_download_bps = $null
            selection = $selection
            capabilities = $capabilities
            observation = $null
            acknowledgement = $null
            supported_actions = @()
            intent = $null
        } | Out-Null
        Wait-ForLog "holding transcode producer"

        Invoke-PlurxJson POST $controlUri @{
            protocol = $session.control.protocol
            generation = $session.control.generation
            control_epoch = $session.control.control_epoch
            client_instance_id = $clientId
            sequence = 2
            demand = "active"
            position_ms = 0
            buffered_from_ms = 0
            buffered_through_ms = 0
            playback_rate = 1
            render_state = "rendering"
            seek_target_ms = $null
            observed_download_bps = $null
            selection = $selection
            capabilities = $null
            observation = $null
            acknowledgement = $null
            supported_actions = @()
            intent = $null
        } | Out-Null
        Wait-ForLog "resuming transcode producer"
    } else {
        throw "HLS session did not advertise playback control"
    }

    if (-not [PlurxSmokeProcess]::GenerateConsoleCtrlEvent(1, [uint32]$server.Id)) {
        throw "GenerateConsoleCtrlEvent failed: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    if (-not $server.WaitForExit(30000)) { throw "plurxd did not exit after Ctrl-C" }
    $server.Refresh()
    if ($server.ExitCode -ne 0) { throw "plurxd exited with $($server.ExitCode) after Ctrl-Break" }

    Start-Sleep -Seconds 1
    $leftovers = @(Get-Process -Name ffmpeg, ffprobe -ErrorAction SilentlyContinue |
        Where-Object { $baselineMediaPids -notcontains $_.Id })
    if ($leftovers.Count -gt 0) {
        throw "job teardown left media children running: $($leftovers.Id -join ', ')"
    }

    [pscustomobject]@{
        result = "pass"
        server = (& $Plurxd --version)
        file_id = $fileId
        session_id = $session.session_id
        direct_range_bytes = 1024
        control_suspend_resume = $true
        clean_child_teardown = $true
    } | ConvertTo-Json -Depth 4
}
finally {
    if ($server -and -not $server.HasExited) {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        $server.WaitForExit()
    }
}
