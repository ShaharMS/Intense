[CmdletBinding()]
param(
    [ValidateRange(2, 1000)]
    [int]$Runs = 10,
    [string]$CorpusRoot = (Join-Path $PSScriptRoot '..\corpus\cache')
)

$manifestPath = Join-Path $PSScriptRoot '..\corpus\manifest.json'
$manifest = Get-Content -Raw $manifestPath | ConvertFrom-Json

$results = foreach ($asset in $manifest.assets) {
    $file = Join-Path $CorpusRoot $asset.name
    if (!(Test-Path -LiteralPath $file)) {
        throw "Missing $($asset.name). Download the SHA-256-pinned corpus asset from $($asset.url)."
    }

    $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant()
    if ($actualHash -ne $asset.sha256) {
        throw "SHA-256 mismatch for $($asset.name): expected $($asset.sha256), got $actualHash."
    }

    $probe = (& ffprobe -v error -select_streams v:0 -show_entries stream=width,height,pix_fmt -of json $file | ConvertFrom-Json).streams[0]
    $samples = [System.Collections.Generic.List[double]]::new()
    for ($run = 1; $run -le $Runs; $run++) {
        $watch = [System.Diagnostics.Stopwatch]::StartNew()
        & ffmpeg -hide_banner -loglevel error -threads 1 -i $file -frames:v 1 -f null - 2>&1 | Out-Null
        $watch.Stop()
        if ($LASTEXITCODE -ne 0) {
            throw "FFmpeg/OpenJPEG decode failed for $($asset.name), run $run."
        }
        $samples.Add([math]::Round($watch.Elapsed.TotalMilliseconds, 3))
    }

    $sorted = @($samples | Sort-Object)
    [pscustomobject]@{
        benchmark = 'ffmpeg-openjpeg-full-decode-single-thread'
        asset = $asset.name
        sha256 = $actualHash
        width = $probe.width
        height = $probe.height
        pixelFormat = $probe.pix_fmt
        runs = $Runs
        medianMs = [math]::Round($sorted[[math]::Floor(($Runs - 1) / 2)], 3)
        p95Ms = $sorted[[math]::Ceiling($Runs * 0.95) - 1]
        minMs = $sorted[0]
        maxMs = $sorted[$Runs - 1]
    }
}

$results | ConvertTo-Json -Depth 4
