# plurx for Windows

This archive contains the native x64 `plurxd.exe` server and an example
configuration. Windows 10 1809, Windows Server 2019, or newer is required.
Keep plurx-managed data and cache directories on NTFS or ReFS.

Install a recent jellyfin-ffmpeg Windows build and either place `ffmpeg.exe`
and `ffprobe.exe` beside `plurxd.exe`, or set the system environment variables
`PLURX_FFMPEG` and `PLURX_FFPROBE` to their absolute paths.

Run in a console:

```powershell
.\plurxd.exe run --config C:\ProgramData\plurx\plurx.toml
```

Install the same command as an automatic LocalSystem service from an elevated
PowerShell window:

```powershell
.\plurxd.exe service install --config C:\ProgramData\plurx\plurx.toml
```

The default service data directory is `%ProgramData%\plurx\data`. Configure
absolute library, data, transcode, and tool paths. Allow TCP 32400 and UDP
32414 through Windows Firewall when clients or discovery cross the host
firewall.

The full install, firewall, upgrade, stop, and uninstall procedures are in
`deploy/README.md` and `docs/OPERATIONS.md` in the source distribution.
