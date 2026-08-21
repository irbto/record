[CmdletBinding()]
param(
    [ValidateRange(3, 500)]
    [int] $Runs = 30,
    [switch] $NoBuild,
    [switch] $Enforce
)

$ErrorActionPreference = "Stop"
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$binary = Join-Path $repositoryRoot "target\release\record.exe"
$probeRoot = Join-Path ([IO.Path]::GetTempPath()) "record-startup-$([guid]::NewGuid().ToString('N'))"

if (-not $NoBuild) {
    & cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw "Release build failed." }
}
if (-not (Test-Path -LiteralPath $binary)) {
    throw "Release binary not found at $binary."
}

function Get-Median([double[]] $Values) {
    $sorted = @($Values | Sort-Object)
    $middle = [Math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2) { return $sorted[$middle] }
    return ($sorted[$middle - 1] + $sorted[$middle]) / 2
}

function Invoke-CaptureProbe {
    $previousProbe = $env:RECORD_INTERNAL_STARTUP_PROBE
    $env:RECORD_INTERNAL_STARTUP_PROBE = "1"
    Push-Location -LiteralPath $probeRoot
    try {
        $text = (& $binary 2>&1 | Out-String)
    } finally {
        Pop-Location
        if ($null -eq $previousProbe) {
            Remove-Item Env:RECORD_INTERNAL_STARTUP_PROBE -ErrorAction SilentlyContinue
        } else {
            $env:RECORD_INTERNAL_STARTUP_PROBE = $previousProbe
        }
    }
    if ($LASTEXITCODE -ne 0) { throw "Capture probe failed:`n$text" }
    if ($text -notmatch 'RECORD_READY_MS=([0-9.]+)') { throw "Capture probe did not report readiness." }
    return [double] $Matches[1]
}

try {
    New-Item -ItemType Directory -Path $probeRoot | Out-Null
    1..3 | ForEach-Object {
        $null = & $binary --version
        $null = Invoke-CaptureProbe
    }

    $cli = [Collections.Generic.List[double]]::new()
    $ready = [Collections.Generic.List[double]]::new()
    1..$Runs | ForEach-Object {
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $null = & $binary --version
        $clock.Stop()
        $cli.Add($clock.Elapsed.TotalMilliseconds)
        $ready.Add((Invoke-CaptureProbe))
    }

    $results = @(
        [pscustomobject]@{
            Metric = "CLI process (--version)"
            MedianMs = [Math]::Round((Get-Median $cli), 2)
            MinMs = [Math]::Round(($cli | Measure-Object -Minimum).Minimum, 2)
            BudgetMs = 50
        }
        [pscustomobject]@{
            Metric = "WASAPI capture ready"
            MedianMs = [Math]::Round((Get-Median $ready), 2)
            MinMs = [Math]::Round(($ready | Measure-Object -Minimum).Minimum, 2)
            BudgetMs = 100
        }
    )
    $results | Format-Table -AutoSize

    if ($Enforce) {
        $failed = @($results | Where-Object { $_.MedianMs -gt $_.BudgetMs })
        if ($failed.Count) { throw "One or more startup budgets were exceeded." }
    }
} finally {
    $systemTemporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    $resolvedProbeRoot = [IO.Path]::GetFullPath($probeRoot)
    if (
        (Test-Path -LiteralPath $resolvedProbeRoot) -and
        $resolvedProbeRoot.StartsWith($systemTemporaryRoot, [StringComparison]::OrdinalIgnoreCase)
    ) {
        Remove-Item -LiteralPath $resolvedProbeRoot -Recurse -Force
    }
}
