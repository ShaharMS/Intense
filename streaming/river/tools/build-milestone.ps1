param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9][a-z0-9.-]{0,63}$')]
    [string] $Milestone,
    # Build a previously benchmarked snapshot without mixing in other trials.
    [string] $SourceRoot,
    # Build and archive a comparison image without changing release aliases.
    [switch] $TrialOnly
)
$ErrorActionPreference = 'Stop'
$riverRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$inputRoot = if ($SourceRoot) { (Resolve-Path -LiteralPath $SourceRoot).Path } else { $riverRoot }
# Every new backend image shares the original-URL streamer contract, including
# images built from frozen snapshots. No channel-specific compatibility fork.
$contractSpec = Get-Content -LiteralPath (Join-Path $inputRoot 'deploy/openapi.json') -Raw | ConvertFrom-Json
$contractMain = Get-Content -LiteralPath (Join-Path $inputRoot 'crates/river-server/src/main.rs') -Raw
if ($contractSpec.paths.PSObject.Properties.Name -notcontains '/api/tiles/{image}' -or -not $contractMain.Contains('get(compatibility::tile_info)')) {
    throw 'Backend images must include the shared tile-info/default streamer contract; apply the common compatibility implementation to this source snapshot first.'
}
$corsSource = Join-Path $inputRoot 'crates/river-server/src/cors.rs'
if (-not (Test-Path -LiteralPath $corsSource) -or -not $contractMain.Contains('cors::expose_response_headers')) {
    throw 'Backend images must include the shared hosting CORS implementation; apply it to every channel/source snapshot before building.'
}
$buildRoot = Join-Path $riverRoot "reports\image-builds\$Milestone"
if (Test-Path -LiteralPath $buildRoot) { throw "A build already exists for $Milestone; use a new milestone name." }
$snapshotRoot = Join-Path $buildRoot 'source'
New-Item -ItemType Directory -Path $snapshotRoot -Force | Out-Null
foreach ($name in @('Cargo.toml', 'Cargo.lock', 'Dockerfile.backend', '.dockerignore', 'crates', 'deploy', 'licenses', 'nvjpeg2000-sidecar', 'web')) {
    if ($name -eq 'deploy') {
        $deploySnapshot = New-Item -ItemType Directory -Path (Join-Path $snapshotRoot 'deploy')
        Get-ChildItem -LiteralPath (Join-Path $inputRoot 'deploy') -Force |
            Where-Object { $_.Name -ne 'local-state' } |
            ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $deploySnapshot.FullName -Recurse }
    } else {
        Copy-Item -LiteralPath (Join-Path $inputRoot $name) -Destination $snapshotRoot -Recurse
    }
}
# The server embeds this file at compile time. Stamp the frozen copy before
# hashing it so the served spec and the image tag share the same identity.
$frozenSpecPath = Join-Path $snapshotRoot 'deploy/openapi.json'
$frozenSpec = Get-Content -LiteralPath $frozenSpecPath -Raw | ConvertFrom-Json
$frozenSpec.info.version = $Milestone
[System.IO.File]::WriteAllText($frozenSpecPath, ($frozenSpec | ConvertTo-Json -Depth 100) + [Environment]::NewLine)

$imageTag = "river:$Milestone"
$statusPath = Join-Path $buildRoot 'status.json'
$sources = Get-ChildItem -LiteralPath $snapshotRoot -Recurse -File | ForEach-Object {
    @{ path = $_.FullName.Substring($snapshotRoot.Length + 1); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
}
@{ milestone = $Milestone; tag = $imageTag; startedAt = [DateTimeOffset]::UtcNow.ToString('o'); sources = @($sources) } |
    ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $buildRoot 'manifest.json') -Encoding utf8
@{ state = 'building'; tag = $imageTag } | ConvertTo-Json | Set-Content -LiteralPath $statusPath -Encoding utf8
$stdoutPath = Join-Path $buildRoot 'build.log'
$stderrPath = Join-Path $buildRoot 'build-errors.log'
$arguments = @('build', '--progress=plain', '-f', 'Dockerfile.backend', '-t', $imageTag, '.')
Push-Location -LiteralPath $snapshotRoot
try {
    $process = Start-Process -FilePath 'docker' -ArgumentList $arguments -WorkingDirectory $snapshotRoot -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
    if ($process.ExitCode -ne 0) {
        @{ state = 'failed'; tag = $imageTag; exitCode = $process.ExitCode } | ConvertTo-Json | Set-Content -LiteralPath $statusPath -Encoding utf8
        throw "Image build failed; see $stderrPath"
    }
    # Do not create an extra runtime-validation container: the six Compose
    # slots are a hard limit. Validate actual runtime files/health when this
    # image is staged into a replica with tools/river-compose.py slot.
    $metadata = docker image inspect $imageTag | ConvertFrom-Json
    $archivePath = Join-Path $buildRoot "$Milestone.tar"
    $compressedPath = "$archivePath.gz"
    docker image save --output $archivePath $imageTag
    if ($LASTEXITCODE -ne 0) { throw 'Image archive export failed.' }
    $inputArchive = [System.IO.File]::OpenRead($archivePath)
    $outputArchive = [System.IO.File]::Create($compressedPath)
    $gzipArchive = [System.IO.Compression.GZipStream]::new($outputArchive, [System.IO.Compression.CompressionLevel]::Optimal)
    try { $inputArchive.CopyTo($gzipArchive) } finally { $gzipArchive.Dispose(); $outputArchive.Dispose(); $inputArchive.Dispose() }
    $archiveHash = (Get-FileHash -LiteralPath $compressedPath -Algorithm SHA256).Hash.ToLowerInvariant()
    "$archiveHash  $([System.IO.Path]::GetFileName($compressedPath))" | Set-Content -LiteralPath "$compressedPath.sha256" -Encoding ascii
    $resolvedArchive = (Resolve-Path -LiteralPath $archivePath).Path
    if (-not $resolvedArchive.StartsWith($buildRoot + [System.IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Archive path escaped the milestone directory.' }
    Remove-Item -LiteralPath $resolvedArchive
    @{ state = 'built'; tag = $imageTag; imageId = $metadata[0].Id; bytes = $metadata[0].Size; archive = $compressedPath; archiveBytes = (Get-Item -LiteralPath $compressedPath).Length; archiveSha256 = $archiveHash; completedAt = [DateTimeOffset]::UtcNow.ToString('o') } |
        ConvertTo-Json | Set-Content -LiteralPath $statusPath -Encoding utf8
    Write-Output "Built $imageTag ($($metadata[0].Size) bytes). Frozen source and logs: $buildRoot"
    if ($TrialOnly) { return }
    # A successful experimental build advances Latest only. Qualification is
    # required separately before changing Stable or LTS.
    docker image tag $imageTag river:latest
    if ($LASTEXITCODE -ne 0) { throw 'Could not advance the Latest alias.' }
    $releasePath = Join-Path $riverRoot 'deploy/releases.json'
    if (Test-Path -LiteralPath $releasePath) {
        $release = Get-Content -LiteralPath $releasePath -Raw | ConvertFrom-Json
        $release | Add-Member -NotePropertyName latest -NotePropertyValue $imageTag -Force
        $release | Add-Member -NotePropertyName latestImageId -NotePropertyValue $metadata[0].Id -Force
        $release | Add-Member -NotePropertyName latestVerified -NotePropertyValue $false -Force
        $release | Add-Member -NotePropertyName latestQualificationEvidence -NotePropertyValue "reports/image-builds/$Milestone/status.json" -Force
        $release | Add-Member -NotePropertyName latestQualificationStatus -NotePropertyValue 'Experimental build; runtime correctness and performance qualification pending.' -Force
        $latestCheckpoint = if ($Milestone -match '-m(\d+)(?:-|$)') { [int]$Matches[1] } else { $null }
        $release | Add-Member -NotePropertyName latestCheckpoint -NotePropertyValue $latestCheckpoint -Force
        $release | Add-Member -NotePropertyName latestUpdatedAt -NotePropertyValue ([DateTimeOffset]::UtcNow.ToString('o')) -Force
        [System.IO.File]::WriteAllText($releasePath, ($release | ConvertTo-Json -Depth 100), [System.Text.UTF8Encoding]::new($false))
        $suitePath = Join-Path $riverRoot 'deploy/benchmark-suite.json'
        if (Test-Path -LiteralPath $suitePath) {
            $suite = Get-Content -LiteralPath $suitePath -Raw | ConvertFrom-Json
            $suite | Add-Member -NotePropertyName latestRelease -NotePropertyValue $latestCheckpoint -Force
            $suite | Add-Member -NotePropertyName latestVerified -NotePropertyValue $false -Force
            $suite | Add-Member -NotePropertyName latestReleaseEvidence -NotePropertyValue "reports/image-builds/$Milestone/status.json" -Force
            $suite | Add-Member -NotePropertyName currentVerdict -NotePropertyValue "Stable $($release.checkpoint), LTS $($release.ltsCheckpoint); Latest $latestCheckpoint is unqualified. See deploy/releases.json for current qualification; prior reports remain historical evidence." -Force
            [System.IO.File]::WriteAllText($suitePath, ($suite | ConvertTo-Json -Depth 100), [System.Text.UTF8Encoding]::new($false))
        }
    }
    $retentionTool = Join-Path $riverRoot 'tools/retain-docker-images.py'
    if (Test-Path -LiteralPath $retentionTool) {
        & python $retentionTool --apply --report (Join-Path $buildRoot 'image-retention.json')
        if ($LASTEXITCODE -ne 0) { throw 'Image built, but image retention needs attention; inspect image-retention.json.' }
    }
} finally { Pop-Location }
