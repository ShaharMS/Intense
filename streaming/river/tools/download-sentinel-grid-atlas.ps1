#requires -Version 7.0

param(
  [string]$SourceAtlas = (Join-Path $PSScriptRoot '..\corpus\cache\sentinel-diverse-atlas\atlas.json'),
  [string]$Destination = (Join-Path $PSScriptRoot '..\corpus\cache\sentinel-grid-atlas'),
  [int]$DownloadConcurrency = 4,
  [double]$MaxCloudPercentage = 55,
  [double]$MinCoveragePercentage = 70,
  [double]$MinMeanLuma = 40,
  [double]$MinP10Luma = 8,
  [double]$MaxDarkPixelPercentage = 70,
  [int]$MaxScenes = 0,
  [switch]$PlanOnly,
  [switch]$ForceRescan
)

$ErrorActionPreference = 'Stop'
if ($DownloadConcurrency -le 0) { throw 'DownloadConcurrency must be positive.' }
if ($MaxScenes -lt 0) { throw 'MaxScenes cannot be negative.' }

$baseUrl = 'https://sentinel-s2-l1c.s3.amazonaws.com'
$sourceManifestPath = [System.IO.Path]::GetFullPath($SourceAtlas)
$root = [System.IO.Path]::GetFullPath($Destination)
$manifestPath = Join-Path $root 'atlas.json'
$logPath = Join-Path $root 'download.log'

# These are compact regional MGRS grids. A primary tile uses the nearest
# available exact-date tile in its own grid as the spatial companion.
$gridPositions = @{
  'west-coast-10S' = @('10/S/DG', '10/S/EG', '10/S/FG', '10/S/GG')
  'central-europe-31U' = @('31/U/DP', '31/U/EP', '31/U/FP', '31/U/GP')
  'east-america-17R' = @('17/R/KH', '17/R/KJ', '17/R/KK', '17/R/KL')
  'asia-50Q' = @('50/Q/KE', '50/Q/KF', '50/Q/KG', '50/Q/KH')
}

if (-not (Test-Path -LiteralPath $sourceManifestPath)) { throw "Missing source atlas manifest: $sourceManifestPath" }
New-Item -ItemType Directory -Force -Path $root | Out-Null
$sourceManifest = Get-Content -Raw -LiteralPath $sourceManifestPath | ConvertFrom-Json
if (@($sourceManifest.scenes).Count -eq 0) { throw 'Source atlas manifest has no scenes.' }

$httpClient = [System.Net.Http.HttpClient]::new()
$httpClient.Timeout = [TimeSpan]::FromSeconds(60)
$httpClient.DefaultRequestHeaders.UserAgent.ParseAdd('River Sentinel grid companion downloader/1.0')

function Get-RemoteText([string]$Url) {
  try { return $httpClient.GetStringAsync($Url).GetAwaiter().GetResult() }
  catch { return $null }
}

function Get-RemoteBytes([string]$Url) {
  try {
    $bytes = $httpClient.GetByteArrayAsync($Url).GetAwaiter().GetResult()
    return ,$bytes
  }
  catch { return $null }
}

function Get-RemoteHead([string]$Url) {
  $request = $null
  $response = $null
  try {
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::Head, $Url)
    $response = $httpClient.SendAsync($request).GetAwaiter().GetResult()
    if (-not $response.IsSuccessStatusCode) { return $null }
    $length = $response.Content.Headers.ContentLength
    if ($null -eq $length) { return $null }
    $etag = $null
    if ($null -ne $response.Headers.ETag) { $etag = $response.Headers.ETag.Tag }
    return [pscustomobject]@{ bytes = [int64]$length; etag = $etag }
  }
  catch { return $null }
  finally {
    if ($null -ne $response) { $response.Dispose() }
    if ($null -ne $request) { $request.Dispose() }
  }
}

function Get-PreviewStats([byte[]]$Bytes) {
  if ($null -eq $Bytes -or $Bytes.Length -eq 0) { return $null }
  $stream = $null
  $bitmap = $null
  try {
    $stream = [System.IO.MemoryStream]::new($Bytes)
    $bitmap = [System.Drawing.Bitmap]::new($stream)
    $lumas = [System.Collections.Generic.List[double]]::new()
    $sum = 0.0
    $dark = 0
    for ($y = 0; $y -lt $bitmap.Height; $y += 4) {
      for ($x = 0; $x -lt $bitmap.Width; $x += 4) {
        $pixel = $bitmap.GetPixel($x, $y)
        $luma = 0.2126 * $pixel.R + 0.7152 * $pixel.G + 0.0722 * $pixel.B
        $lumas.Add($luma)
        $sum += $luma
        if ($luma -lt 25.0) { $dark++ }
      }
    }
    $sorted = $lumas.ToArray()
    [array]::Sort($sorted)
    $p10Index = [int][math]::Floor(($sorted.Count - 1) * 0.10)
    return [pscustomobject]@{
      width = $bitmap.Width
      height = $bitmap.Height
      meanLuma = [math]::Round($sum / $sorted.Count, 3)
      p10Luma = [math]::Round($sorted[$p10Index], 3)
      darkPixelPercentage = [math]::Round(100.0 * $dark / $sorted.Count, 3)
    }
  }
  catch { return $null }
  finally {
    if ($null -ne $bitmap) { $bitmap.Dispose() }
    if ($null -ne $stream) { $stream.Dispose() }
  }
}

function Write-JsonAtomic($Value, [string]$Path) {
  $partial = "$Path.partial"
  $json = $Value | ConvertTo-Json -Depth 12
  Set-Content -LiteralPath $partial -Value $json -Encoding utf8
  Move-Item -Force -LiteralPath $partial -Destination $Path
}

function Write-TextUtf8([string]$Path, [string]$Text) {
  $encoding = [System.Text.UTF8Encoding]::new($false)
  [System.IO.File]::WriteAllText($Path, $Text, $encoding)
}

function Get-GridCandidate($Scene, [string]$NeighborPath) {
  $date = [datetime]::ParseExact([string]$Scene.acquired, 'yyyy-MM-dd', [Globalization.CultureInfo]::InvariantCulture)
  $datePath = $date.ToString('yyyy/M/d')
  $tileId = ($NeighborPath -replace '/', '')
  $tileBase = "$baseUrl/tiles/$NeighborPath/$datePath/0"
  $infoJson = Get-RemoteText "$tileBase/tileInfo.json"
  if ([string]::IsNullOrWhiteSpace($infoJson)) { return $null }
  try { $info = $infoJson | ConvertFrom-Json } catch { return $null }

  $cloud = [double]$info.cloudyPixelPercentage
  $coverage = [double]$info.dataCoveragePercentage
  if ($coverage -lt $MinCoveragePercentage -or $cloud -gt $MaxCloudPercentage) { return $null }
  $previewBytes = Get-RemoteBytes "$tileBase/preview.jpg"
  $previewStats = Get-PreviewStats $previewBytes
  if ($null -eq $previewStats) { return $null }
  if ($previewStats.meanLuma -lt $MinMeanLuma -or $previewStats.p10Luma -lt $MinP10Luma -or $previewStats.darkPixelPercentage -gt $MaxDarkPixelPercentage) { return $null }

  $objects = @()
  foreach ($band in @('B04', 'B03', 'B02')) {
    $url = "$tileBase/$band.jp2"
    $head = Get-RemoteHead $url
    if ($null -eq $head) { return $null }
    $objects += [pscustomobject]@{ band = $band; url = $url; bytes = [int64]$head.bytes; etag = $head.etag }
  }
  return [pscustomobject]@{
    pairId = "grid-$($Scene.sceneId)-$tileId"
    acquired = $Scene.acquired
    primarySceneId = $Scene.sceneId
    primaryTile = $Scene.tile.tile
    neighbor = [pscustomobject]@{
      tile = $NeighborPath
      tileId = $tileId
      quality = [pscustomobject]@{
        cloudyPixelPercentage = $cloud
        dataCoveragePercentage = $coverage
        meanLuma = $previewStats.meanLuma
        p10Luma = $previewStats.p10Luma
        darkPixelPercentage = $previewStats.darkPixelPercentage
      }
    }
    infoJson = $infoJson
    previewBytes = $previewBytes
    objects = $objects
  }
}

function New-ManifestPair($Candidate) {
  $sceneObjects = @()
  foreach ($object in $Candidate.objects) {
    $relativePath = "$($Candidate.pairId)/$($Candidate.neighbor.tileId)/$($object.band).jp2"
    $sceneObjects += [ordered]@{
      tile = $Candidate.neighbor.tile
      tileId = $Candidate.neighbor.tileId
      band = $object.band
      url = $object.url
      etag = $object.etag
      bytes = [int64]$object.bytes
      path = $relativePath
    }
  }
  return [ordered]@{
    pairId = $Candidate.pairId
    acquired = $Candidate.acquired
    primarySceneId = $Candidate.primarySceneId
    primaryTile = $Candidate.primaryTile
    neighbor = [ordered]@{
      tile = $Candidate.neighbor.tile
      tileId = $Candidate.neighbor.tileId
      quality = [ordered]@{
        cloudyPixelPercentage = $Candidate.neighbor.quality.cloudyPixelPercentage
        dataCoveragePercentage = $Candidate.neighbor.quality.dataCoveragePercentage
        meanLuma = $Candidate.neighbor.quality.meanLuma
        p10Luma = $Candidate.neighbor.quality.p10Luma
        darkPixelPercentage = $Candidate.neighbor.quality.darkPixelPercentage
      }
    }
    objects = $sceneObjects
  }
}

function Write-SelectedEvidence($Pair, $Candidate) {
  $tileEvidence = Join-Path (Join-Path (Join-Path $root 'evidence') $Pair.pairId) $Candidate.neighbor.tileId
  New-Item -ItemType Directory -Force -Path $tileEvidence | Out-Null
  [System.IO.File]::WriteAllBytes((Join-Path $tileEvidence 'preview.jpg'), [byte[]]$Candidate.previewBytes)
  Write-TextUtf8 (Join-Path $tileEvidence 'tileInfo.json') $Candidate.infoJson
}

try {
  if ((Test-Path -LiteralPath $manifestPath) -and -not $ForceRescan) {
    $manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if (-not ($manifest.PSObject.Properties.Name -contains 'completedAt')) {
      $manifest | Add-Member -NotePropertyName completedAt -NotePropertyValue $null
    }
    if (@($manifest.pairs).Count -eq 0) { throw "Existing grid manifest '$manifestPath' has no pairs." }
    Write-Host "Reusing existing grid plan: $(@($manifest.pairs).Count) pairs, $([math]::Round([double]$manifest.plannedBytes / 1GB, 2)) GiB planned."
  }
  else {
    if ($ForceRescan -and @(Get-ChildItem -LiteralPath $root -Recurse -File -Filter '*.jp2' -ErrorAction SilentlyContinue).Count -gt 0) {
      throw 'ForceRescan requires an empty destination or a new Destination because the old grid plan would be orphaned.'
    }
    $pairs = [System.Collections.Generic.List[object]]::new()
    $candidates = [System.Collections.Generic.List[object]]::new()
    $skipped = [System.Collections.Generic.List[object]]::new()
    foreach ($scene in @($sourceManifest.scenes)) {
      if ($MaxScenes -gt 0 -and $candidates.Count -ge $MaxScenes) { break }
      $positions = $gridPositions[[string]$scene.strip]
      if ($null -eq $positions) {
        $skipped.Add([pscustomobject]@{ primarySceneId = $scene.sceneId; reason = 'unknown_strip' })
        continue
      }
      $primaryTileId = [string]$scene.tile.tileId
      $primaryIndex = -1
      for ($i = 0; $i -lt $positions.Count; $i++) {
        if (($positions[$i] -replace '/', '') -eq $primaryTileId) { $primaryIndex = $i; break }
      }
      if ($primaryIndex -lt 0) {
        $skipped.Add([pscustomobject]@{ primarySceneId = $scene.sceneId; reason = 'primary_not_in_grid' })
        continue
      }

      $offsets = @(-1, 1, -2, 2, -3, 3)
      $candidate = $null
      foreach ($offset in $offsets) {
        $neighborIndex = $primaryIndex + $offset
        if ($neighborIndex -lt 0 -or $neighborIndex -ge $positions.Count) { continue }
        $neighborPath = $positions[$neighborIndex]
        $candidate = Get-GridCandidate $scene $neighborPath
        if ($null -ne $candidate) { break }
      }
      if ($null -eq $candidate) {
        $skipped.Add([pscustomobject]@{ primarySceneId = $scene.sceneId; reason = 'no_exact_date_quality_neighbor' })
        continue
      }
      $candidates.Add($candidate)
      Write-Host ("  grid pair {0}: {1} -> {2}" -f $candidate.acquired, $candidate.primaryTile, $candidate.neighbor.tile)
    }
    if ($candidates.Count -eq 0) { throw 'No quality-qualified exact-date grid companions were found.' }

    $usedDates = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($candidate in $candidates) {
      if (-not $usedDates.Add($candidate.acquired)) {
        throw "Grid date uniqueness check failed for $($candidate.acquired)."
      }
      $pairs.Add((New-ManifestPair $candidate))
    }
    $plannedBytes = [int64](($candidates | ForEach-Object { $_.objects } | Measure-Object -Property bytes -Sum).Sum)
    $manifest = [ordered]@{
      schemaVersion = 1
      title = 'River Sentinel-2 exact-date neighboring-grid companion atlas'
      source = 'Sentinel-2 Level-1C public S3 archive'
      sourceAtlas = $sourceManifestPath
      layout = 'pairId/neighborTileId/B04.jp2, B03.jp2, B02.jp2'
      policy = 'one quality-qualified neighboring MGRS tile per primary scene; primary files are referenced, not copied'
      primarySceneCount = @($sourceManifest.scenes).Count
      pairCount = $pairs.Count
      skippedCount = $skipped.Count
      targetDateUniqueness = $true
      quality = [ordered]@{
        maxCloudPercentage = $MaxCloudPercentage
        minCoveragePercentage = $MinCoveragePercentage
        minMeanLuma = $MinMeanLuma
        minP10Luma = $MinP10Luma
        maxDarkPixelPercentage = $MaxDarkPixelPercentage
      }
      plannedBytes = $plannedBytes
      downloadedBytes = 0
      downloadState = 'planned'
      generatedAt = (Get-Date).ToUniversalTime().ToString('o')
      completedAt = $null
      pairs = $pairs
      skipped = $skipped
    }
    Write-JsonAtomic $manifest $manifestPath
    Write-Host "Wrote grid plan: $manifestPath ($($pairs.Count) pairs, $([math]::Round($plannedBytes / 1GB, 2)) GiB planned; skipped $($skipped.Count))."
    foreach ($candidate in $candidates) {
      $pair = $pairs | Where-Object { $_.pairId -eq $candidate.pairId } | Select-Object -First 1
      Write-SelectedEvidence $pair $candidate
    }
  }

  if ($PlanOnly) {
    Write-Host 'PlanOnly requested; no grid JP2 files were downloaded.'
    exit 0
  }

  $active = [System.Collections.Generic.List[object]]::new()
  function Start-CurlDownload($Object, [string]$Path) {
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = 'curl.exe'
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.RedirectStandardOutput = $true
    foreach ($argument in @('--fail', '--location', '--retry', '5', '--retry-all-errors', '--connect-timeout', '30', '--speed-time', '120', '--speed-limit', '4096', '--continue-at', '-', '--silent', '--show-error', '--output', $Path, $Object.url)) {
      [void]$psi.ArgumentList.Add([string]$argument)
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $psi
    [void]$process.Start()
    return [pscustomobject]@{ process = $process; object = $Object; path = $Path }
  }

  function Complete-CurlDownload($Job) {
    $stderr = $Job.process.StandardError.ReadToEnd()
    $Job.process.WaitForExit()
    if ($Job.process.ExitCode -ne 0) { throw "Download failed ($($Job.process.ExitCode)) for $($Job.object.url): $stderr" }
    if (-not (Test-Path -LiteralPath $Job.path)) { throw "Download produced no file: $($Job.path)" }
    $actual = (Get-Item -LiteralPath $Job.path).Length
    if ($actual -ne [int64]$Job.object.bytes) { throw "Size mismatch for $($Job.path): got $actual, expected $($Job.object.bytes)." }
    Add-Content -LiteralPath $logPath -Encoding utf8 -Value "$(Get-Date -Format o) complete $($Job.object.url) $actual bytes"
    Write-Host ("  complete {0} ({1:N2} GiB)" -f $Job.object.path, ($actual / 1GB))
    $Job.process.Dispose()
  }

  function Wait-ForDownloadSlot {
    while ($active.Count -ge $DownloadConcurrency) {
      $finished = $null
      foreach ($job in @($active)) { if ($job.process.HasExited) { $finished = $job; break } }
      if ($null -eq $finished) { Start-Sleep -Milliseconds 500; continue }
      Complete-CurlDownload $finished
      [void]$active.Remove($finished)
    }
  }

  try {
    foreach ($pair in @($manifest.pairs)) {
      foreach ($object in @($pair.objects)) {
        $relativeWindowsPath = ([string]$object.path) -replace '/', '\'
        $path = Join-Path $root $relativeWindowsPath
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
        if (Test-Path -LiteralPath $path) {
          $length = (Get-Item -LiteralPath $path).Length
          if ($length -eq [int64]$object.bytes) { continue }
          if ($length -gt [int64]$object.bytes) { throw "Existing file is larger than its manifest entry: $path" }
        }
        Add-Content -LiteralPath $logPath -Encoding utf8 -Value "$(Get-Date -Format o) start $($object.url) -> $path"
        $active.Add((Start-CurlDownload $object $path))
        Wait-ForDownloadSlot
      }
    }
    while ($active.Count -gt 0) {
      $finished = $null
      foreach ($job in @($active)) { if ($job.process.HasExited) { $finished = $job; break } }
      if ($null -eq $finished) { Start-Sleep -Milliseconds 500; continue }
      Complete-CurlDownload $finished
      [void]$active.Remove($finished)
    }
  }
  catch {
    foreach ($job in @($active)) {
      if (-not $job.process.HasExited) { $job.process.Kill() }
      $job.process.Dispose()
    }
    throw
  }

  $downloadedBytes = [int64]0
  $objectPaths = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
  foreach ($pair in @($manifest.pairs)) {
    foreach ($object in @($pair.objects)) {
      $relativeWindowsPath = ([string]$object.path) -replace '/', '\'
      $path = Join-Path $root $relativeWindowsPath
      if (-not (Test-Path -LiteralPath $path)) { throw "Missing downloaded object: $path" }
      $item = Get-Item -LiteralPath $path
      if ($item.Length -ne [int64]$object.bytes) { throw "Final size mismatch for $path" }
      $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
      $object | Add-Member -NotePropertyName sha256 -NotePropertyValue $hash -Force
      [void]$objectPaths.Add($relativeWindowsPath)
      $downloadedBytes += [int64]$item.Length
    }
  }
  $actualJp2 = @(Get-ChildItem -LiteralPath $root -Recurse -File -Filter '*.jp2' | ForEach-Object { $_.FullName.Substring($root.Length + 1) })
  if ($actualJp2.Count -ne $objectPaths.Count) { throw "Unexpected JP2 count: found $($actualJp2.Count), expected $($objectPaths.Count)." }
  foreach ($path in $actualJp2) { if (-not $objectPaths.Contains($path)) { throw "Unexpected JP2 file in grid corpus: $path" } }

  $manifest.downloadedBytes = $downloadedBytes
  $manifest.downloadState = 'complete'
  $manifest.completedAt = (Get-Date).ToUniversalTime().ToString('o')
  Write-JsonAtomic $manifest $manifestPath
  Add-Content -LiteralPath $logPath -Encoding utf8 -Value "$(Get-Date -Format o) completed $downloadedBytes bytes across $($manifest.pairs.Count) pairs"
  Write-Host "Complete: $($manifest.pairs.Count) grid pairs, $([math]::Round($downloadedBytes / 1GB, 2)) GiB, $($objectPaths.Count) JP2 objects."
}
finally {
  if ($null -ne $httpClient) { $httpClient.Dispose() }
}
