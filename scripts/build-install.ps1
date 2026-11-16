<#
.SYNOPSIS
Builds record and record-desktop from this folder and installs them.

.DESCRIPTION
A running copy locks its files, so this script first asks the running
programs to exit, builds, copies the new files, and starts again what ran
before. A recording that runs saves its file before the program exits.
#>
[CmdletBinding()]
param(
    [string] $InstallDirectory = "$env:LOCALAPPDATA\Programs\record",
    [switch] $NoRestart
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "target" }
$release = Join-Path $targetRoot "release"

function Get-DesktopProcesses {
    @(Get-Process -Name "record-desktop" -ErrorAction SilentlyContinue)
}

function Get-TrayProcesses {
    @(Get-CimInstance Win32_Process -Filter "Name = 'record.exe'" |
        Where-Object { $_.CommandLine -match "--tray" })
}

function Install-File {
    param([string] $Source, [string] $Destination)
    try {
        Copy-Item -LiteralPath $Source -Destination $Destination -Force
    } catch {
        # Windows does not overwrite a file that a running program uses, but
        # it does rename one. The running program keeps the old file.
        $oldName = "$(Split-Path -Leaf $Destination).old-$([guid]::NewGuid().ToString('N'))"
        Rename-Item -LiteralPath $Destination -NewName $oldName
        Copy-Item -LiteralPath $Source -Destination $Destination -Force
    }
}

# Remember what runs, then ask it to exit.
$desktop = Get-DesktopProcesses
$desktopRan = $desktop.Count -gt 0
$tray = Get-TrayProcesses
$trayRan = $tray.Count -gt 0

if ($desktopRan) {
    Write-Host "Closing record-desktop" -ForegroundColor Cyan
    $running = $desktop | Where-Object { $_.Path } | Select-Object -First 1
    if ($running) {
        Start-Process -FilePath $running.Path -ArgumentList "--quit"
    }
    # A recording needs time to close its file. A copy from before --quit
    # existed opens a second window instead, so stop waiting when that shows.
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        $left = Get-DesktopProcesses
        if ($left.Count -eq 0 -or $left.Count -gt $desktop.Count) { break }
    }
    Get-DesktopProcesses | Stop-Process -Force -ErrorAction SilentlyContinue
}
foreach ($process in $tray) {
    Stop-Process -Id $process.ProcessId -Force -ErrorAction SilentlyContinue
}

Push-Location $root
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed for record." }
    cargo build --release -p record-desktop
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed for record-desktop." }
} finally {
    Pop-Location
}

New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
Get-ChildItem -LiteralPath $InstallDirectory -Filter "*.old-*" -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue

# The GNU toolchain puts WebView2Loader.dll next to record-desktop.exe, and
# the program needs it there. The MSVC toolchain links it into the program.
foreach ($name in @("record.exe", "record-desktop.exe", "WebView2Loader.dll")) {
    $source = Join-Path $release $name
    if (Test-Path -LiteralPath $source) {
        Install-File -Source $source -Destination (Join-Path $InstallDirectory $name)
    }
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$entries = @($userPath -split ';' | Where-Object { $_ })
if ($entries -inotcontains $InstallDirectory) {
    [Environment]::SetEnvironmentVariable("Path", ((@($entries) + $InstallDirectory) -join ';'), "User")
}
if (($env:Path -split ';') -inotcontains $InstallDirectory) {
    $env:Path = "$env:Path;$InstallDirectory"
}

Write-Host "Installed to $InstallDirectory" -ForegroundColor Green

if (-not $NoRestart) {
    if ($desktopRan) {
        Start-Process -FilePath (Join-Path $InstallDirectory "record-desktop.exe")
    }
    if ($trayRan) {
        Start-Process -FilePath (Join-Path $InstallDirectory "record.exe") -ArgumentList "screenshot", "--tray"
    }
}
