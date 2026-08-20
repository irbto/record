[CmdletBinding()]
param(
    [string] $Version = "latest",
    [string] $InstallDirectory = "$env:LOCALAPPDATA\Programs\record"
)

$ErrorActionPreference = "Stop"
$repository = "irbto/record"
$assetName = "record-x86_64-pc-windows-msvc.zip"
$apiUrl = if ($Version -eq "latest") {
    "https://api.github.com/repos/$repository/releases/latest"
} else {
    "https://api.github.com/repos/$repository/releases/tags/$Version"
}

$headers = @{ "User-Agent" = "record-installer" }
$release = Invoke-RestMethod -Uri $apiUrl -Headers $headers
$archiveAsset = $release.assets | Where-Object name -eq $assetName | Select-Object -First 1
$checksumAsset = $release.assets | Where-Object name -eq "$assetName.sha256" | Select-Object -First 1
if (-not $archiveAsset -or -not $checksumAsset) {
    throw "Release $($release.tag_name) does not contain the expected Windows assets."
}

$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) "record-$([guid]::NewGuid().ToString('N'))"
$archivePath = Join-Path $temporaryRoot $assetName
$checksumPath = "$archivePath.sha256"

try {
    New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
    Invoke-WebRequest -Uri $archiveAsset.browser_download_url -Headers $headers -OutFile $archivePath
    Invoke-WebRequest -Uri $checksumAsset.browser_download_url -Headers $headers -OutFile $checksumPath

    $expected = (Get-Content -Raw $checksumPath).Trim().Split(' ')[0]
    $actual = (Get-FileHash -Algorithm SHA256 $archivePath).Hash
    if ($actual -ine $expected) {
        throw "SHA-256 verification failed for $assetName."
    }

    $expanded = Join-Path $temporaryRoot "expanded"
    Expand-Archive -LiteralPath $archivePath -DestinationPath $expanded
    New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
    Copy-Item -LiteralPath (Join-Path $expanded "record.exe") -Destination $InstallDirectory -Force

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $entries = @($userPath -split ';' | Where-Object { $_ })
    if ($entries -inotcontains $InstallDirectory) {
        $updatedPath = (@($entries) + $InstallDirectory) -join ';'
        [Environment]::SetEnvironmentVariable("Path", $updatedPath, "User")
    }
    if (($env:Path -split ';') -inotcontains $InstallDirectory) {
        $env:Path = "$env:Path;$InstallDirectory"
    }

    Write-Host "Installed record $($release.tag_name) to $InstallDirectory" -ForegroundColor Green
    Write-Host "Open a new terminal and run: record"
} finally {
    $systemTemporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    $resolvedTemporaryRoot = [IO.Path]::GetFullPath($temporaryRoot)
    if ($resolvedTemporaryRoot.StartsWith($systemTemporaryRoot, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedTemporaryRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
