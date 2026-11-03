#requires -Version 7.0

param(
  [int]$TargetGiB = 50,
  [string]$Destination = (Join-Path $PSScriptRoot '..\corpus\cache\sentinel-diverse-atlas'),
  [string]$ExistingAtlasPath = (Join-Path $PSScriptRoot '..\corpus\cache\sentinel-atlas\atlas.json'),
  [datetime]$StartDate = [datetime]'2017-01-01',
  [datetime]$EndDate = (Get-Date).ToUniversalTime().Date,
  [int]$MaxCandidatesPerTile = 50,
  [int]$DownloadConcurrency = 6,
  [double]$MaxCloudPercentage = 55,
  [double]$MinCoveragePercentage = 70,
  [double]$MinMeanLuma = 40,
  [double]$MinP10Luma = 8,
  [double]$MaxDarkPixelPercentage = 70,
  [switch]$PlanOnly,
  [switch]$ForceRescan
)

$ErrorActionPreference = 'Stop'

if ($TargetGiB -le 0) { throw 'TargetGiB must be positive.' }
if ($StartDate -gt $EndDate) { throw 'StartDate must be on or before EndDate.' }
if ($MaxCandidatesPerTile -le 0) { throw 'MaxCandidatesPerTile must be positive.' }
if ($DownloadConcurrency -le 0) { throw 'DownloadConcurrency must be positive.' }

$baseUrl = 'https://sentinel-s2-l1c.s3.amazonaws.com'
$root = [System.IO.Path]::GetFullPath($Destination)
$manifestPath = Join-Path $root 'atlas.json'
$logPath = Join-Path $root 'download.log'
$targetBytes = [int64]$TargetGiB * 1GB

# Each pair contains nearby source tiles. The four pairs are geographically
# far apart; each selected scene uses one tile and one unique acquisition date.
$strips = @(
  [pscustomobject]@{
    name = 'west-coast-10S'
    tiles = @('10/S/DG', '10/S/EG')
  }
  [pscustomobject]@{
    name = 'central-europe-31U'
    tiles = @('31/U/DP', '31/U/EP')
  }
  [pscustomobject]@{
    name = 'east-america-17R'
    tiles = @('17/R/KH', '17/R/KJ')
  }
  [pscustomobject]@{
    name = 'asia-50Q'
    tiles = @('50/Q/KE', '50/Q/KF')
  }
)

New-Item -ItemType Directory -Force -Path $root | Out-Null

$httpClient = [System.Net.Http.HttpClient]::new()
$httpClient.Timeout = [TimeSpan]::FromSeconds(60)
$httpClient.DefaultRequestHeaders.UserAgent.ParseAdd('River Sentinel corpus downloader/1.0')

function Get-RemoteText([string]$Url) {
  try {
    return $httpClient.GetStringAsync($Url).GetAwaiter().GetResult()
  }
  catch {
    return $null
  }
}

function Get-RemoteBytes([string]$Url) {
  try {
    $bytes = $httpClient.GetByteArrayAsync($Url).GetAwaiter().GetResult()
    return ,$bytes
  }
  catch {
    return $null
  }
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
    return [pscustomobject]@{
      bytes = [int64]$length
      etag = $etag
    }
  }
  catch {
    return $null
  }
  finally {
    if ($null -ne $response) { $response.Dispose() }
    if ($null -ne $request) { $request.Dispose() }
  }
}

function Get-S3CommonPrefixes([string]$Prefix) {
  $encodedPrefix = [System.Uri]::EscapeDataString($Prefix)
  $url = "$baseUrl/?list-type=2&prefix=$encodedPrefix&delimiter=%2F"
  $raw = Get-RemoteText $url
  if ([string]::IsNullOrWhiteSpace($raw)) { return @() }
  try {
    $xml = [xml]$raw
    if ($null -eq $xml.ListBucketResult.CommonPrefixes) { return @() }
    return @($xml.ListBucketResult.CommonPrefixes | ForEach-Object { [string]$_.Prefix })
  }
  catch {
    return @()
  }
}

function Get-AvailableDates([string]$TilePath) {
  $result = [System.Collections.Generic.List[datetime]]::new()
  for ($year = $StartDate.Year; $year -le $EndDate.Year; $year++) {
    $yearPrefix = "tiles/$TilePath/$year/"
    foreach ($monthPrefix in @(Get-S3CommonPrefixes $yearPrefix)) {
      foreach ($dayPrefix in @(Get-S3CommonPrefixes $monthPrefix)) {
        $parts = $dayPrefix.TrimEnd('/') -split '/'
        if ($parts.Count -lt 3) { continue }
        try {
          $date = [datetime]::new([int]$parts[-3], [int]$parts[-2], [int]$parts[-1])
          if ($date -ge $StartDate.Date -and $date -le $EndDate.Date) {
            $result.Add($date.Date)
          }
        }
        catch {
          # Ignore malformed prefixes from the public listing.
        }
      }
    }
  }
  return @($result | Sort-Object -Unique)
}

function Convert-UtmToLatLon([double]$Easting, [double]$Northing, [int]$Zone, [string]$LatitudeBand) {
  $a = 6378137.0
  $eccSquared = 0.00669438
  $k0 = 0.9996
  $eccPrimeSquared = $eccSquared / (1.0 - $eccSquared)

  $isNorthern = ([int][char]$LatitudeBand[0]) -ge ([int][char]'N')
  $northingForFormula = $Northing
  if (-not $isNorthern) { $northingForFormula -= 10000000.0 }

  $x = $Easting - 500000.0
  $y = $northingForFormula
  $m = $y / $k0
  $mu = $m / ($a * (1.0 - $eccSquared / 4.0 - 3.0 * [math]::Pow($eccSquared, 2) / 64.0 - 5.0 * [math]::Pow($eccSquared, 3) / 256.0))
  $e1 = (1.0 - [math]::Sqrt(1.0 - $eccSquared)) / (1.0 + [math]::Sqrt(1.0 - $eccSquared))
  $phi1 = $mu +
    (3.0 * $e1 / 2.0 - 27.0 * [math]::Pow($e1, 3) / 32.0) * [math]::Sin(2.0 * $mu) +
    (21.0 * [math]::Pow($e1, 2) / 16.0 - 55.0 * [math]::Pow($e1, 4) / 32.0) * [math]::Sin(4.0 * $mu) +
    (151.0 * [math]::Pow($e1, 3) / 96.0) * [math]::Sin(6.0 * $mu) +
    (1097.0 * [math]::Pow($e1, 4) / 512.0) * [math]::Sin(8.0 * $mu)

  $sinPhi = [math]::Sin($phi1)
  $cosPhi = [math]::Cos($phi1)
  $tanPhi = [math]::Tan($phi1)
  $n1 = $a / [math]::Sqrt(1.0 - $eccSquared * [math]::Pow($sinPhi, 2))
  $t1 = [math]::Pow($tanPhi, 2)
  $c1 = $eccPrimeSquared * [math]::Pow($cosPhi, 2)
  $r1 = $a * (1.0 - $eccSquared) / [math]::Pow(1.0 - $eccSquared * [math]::Pow($sinPhi, 2), 1.5)
  $d = $x / ($n1 * $k0)

  $latitude = $phi1 - ($n1 * $tanPhi / $r1) * (
    [math]::Pow($d, 2) / 2.0 -
    (5.0 + 3.0 * $t1 + 10.0 * $c1 - 4.0 * [math]::Pow($c1, 2) - 9.0 * $eccPrimeSquared) * [math]::Pow($d, 4) / 24.0 +
    (61.0 + 90.0 * $t1 + 298.0 * $c1 + 45.0 * [math]::Pow($t1, 2) - 252.0 * $eccPrimeSquared - 3.0 * [math]::Pow($c1, 2)) * [math]::Pow($d, 6) / 720.0
  )
  $longitude = (
    $d - (1.0 + 2.0 * $t1 + $c1) * [math]::Pow($d, 3) / 6.0 +
    (5.0 - 2.0 * $c1 + 28.0 * $t1 - 3.0 * [math]::Pow($c1, 2) + 8.0 * $eccPrimeSquared + 24.0 * [math]::Pow($t1, 2)) * [math]::Pow($d, 5) / 120.0
  ) / $cosPhi

  $longitudeOrigin = ($Zone - 1) * 6 - 180 + 3
  return [pscustomobject]@{
    latitude = $latitude * 180.0 / [math]::PI
    longitude = $longitudeOrigin + $longitude * 180.0 / [math]::PI
  }
}

function Get-TileCenter($TileInfo) {
  if ($null -eq $TileInfo.tileGeometry -or $null -eq $TileInfo.tileGeometry.coordinates) { return $null }
  $ring = @($TileInfo.tileGeometry.coordinates[0])
  if ($ring.Count -lt 4) { return $null }
  $xs = @($ring | ForEach-Object { [double]$_[0] })
  $ys = @($ring | ForEach-Object { [double]$_[1] })
  $easting = (($xs | Measure-Object -Minimum).Minimum + ($xs | Measure-Object -Maximum).Maximum) / 2.0
  $northing = (($ys | Measure-Object -Minimum).Minimum + ($ys | Measure-Object -Maximum).Maximum) / 2.0
  return Convert-UtmToLatLon $easting $northing ([int]$TileInfo.utmZone) ([string]$TileInfo.latitudeBand)
}

function Get-HaversineKm($A, $B) {
  $rad = [math]::PI / 180.0
  $dLat = ($B.latitude - $A.latitude) * $rad
  $dLon = ($B.longitude - $A.longitude) * $rad
  $lat1 = $A.latitude * $rad
  $lat2 = $B.latitude * $rad
  $h = [math]::Pow([math]::Sin($dLat / 2.0), 2) + [math]::Cos($lat1) * [math]::Cos($lat2) * [math]::Pow([math]::Sin($dLon / 2.0), 2)
  return 6371.0088 * 2.0 * [math]::Atan2([math]::Sqrt($h), [math]::Sqrt(1.0 - $h))
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
    $sampleStep = 4
    for ($y = 0; $y -lt $bitmap.Height; $y += $sampleStep) {
      for ($x = 0; $x -lt $bitmap.Width; $x += $sampleStep) {
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
  catch {
    return $null
  }
  finally {
    if ($null -ne $bitmap) { $bitmap.Dispose() }
    if ($null -ne $stream) { $stream.Dispose() }
  }
}

function Get-ExistingDates([string]$Path) {
  $dates = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
  if (-not (Test-Path -LiteralPath $Path)) { return $dates }
  try {
    $manifest = Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
    foreach ($scene in @($manifest.scenes)) {
      if (-not [string]::IsNullOrWhiteSpace([string]$scene.acquired)) { [void]$dates.Add([string]$scene.acquired) }
    }
  }
  catch {
    throw "Could not read existing atlas manifest '$Path': $($_.Exception.Message)"
  }
  return $dates
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

function Write-SelectedEvidence($Scene, $Candidate) {
  $sceneEvidence = Join-Path (Join-Path $root 'evidence') $Scene.sceneId
  $tileEvidence = Join-Path $sceneEvidence $Candidate.tile.tileId
  New-Item -ItemType Directory -Force -Path $tileEvidence | Out-Null
  [System.IO.File]::WriteAllBytes((Join-Path $tileEvidence 'preview.jpg'), [byte[]]$Candidate.tile.previewBytes)
  Write-TextUtf8 (Join-Path $tileEvidence 'tileInfo.json') $Candidate.tile.infoJson
}

function Get-TileCandidate([string]$StripName, [string]$TilePath, [datetime]$Date) {
  $dateText = $Date.ToString('yyyy-MM-dd')
  $datePath = $Date.ToString('yyyy/M/d')
  $tileId = ($TilePath -replace '/', '')
  $tileBase = "$baseUrl/tiles/$TilePath/$datePath/0"
  $infoUrl = "$tileBase/tileInfo.json"
  $infoJson = Get-RemoteText $infoUrl
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
    $objects += [pscustomobject]@{
      band = $band
      url = $url
      bytes = [int64]$head.bytes
      etag = $head.etag
    }
  }

  $center = Get-TileCenter $info
  if ($null -eq $center) { return $null }
  $sceneBytes = [int64](($objects | Measure-Object -Property bytes -Sum).Sum)
  return [pscustomobject]@{
    sceneId = "$dateText-$tileId"
    acquired = $dateText
    strip = $StripName
    tile = [pscustomobject]@{
      tilePath = $TilePath
      tileId = $tileId
      infoJson = $infoJson
      info = $info
      center = $center
      previewBytes = $previewBytes
      quality = [pscustomobject]@{
        cloudyPixelPercentage = $cloud
        dataCoveragePercentage = $coverage
        meanLuma = $previewStats.meanLuma
        p10Luma = $previewStats.p10Luma
        darkPixelPercentage = $previewStats.darkPixelPercentage
      }
      objects = $objects
    }
    bytes = $sceneBytes
  }
}

function New-ManifestScene($Candidate) {
  $sceneObjects = @()
  foreach ($object in $Candidate.tile.objects) {
    $relativePath = "$($Candidate.sceneId)/$($Candidate.tile.tileId)/$($object.band).jp2"
    $sceneObjects += [ordered]@{
      tile = $Candidate.tile.tilePath
      tileId = $Candidate.tile.tileId
      band = $object.band
      url = $object.url
      etag = $object.etag
      bytes = [int64]$object.bytes
      path = $relativePath
    }
  }
  return [ordered]@{
    sceneId = $Candidate.sceneId
    acquired = $Candidate.acquired
    strip = $Candidate.strip
    center = [ordered]@{ latitude = $Candidate.tile.center.latitude; longitude = $Candidate.tile.center.longitude }
    tile = [ordered]@{
      tile = $Candidate.tile.tilePath
      tileId = $Candidate.tile.tileId
      quality = [ordered]@{
        cloudyPixelPercentage = $Candidate.tile.quality.cloudyPixelPercentage
        dataCoveragePercentage = $Candidate.tile.quality.dataCoveragePercentage
        meanLuma = $Candidate.tile.quality.meanLuma
        p10Luma = $Candidate.tile.quality.p10Luma
        darkPixelPercentage = $Candidate.tile.quality.darkPixelPercentage
      }
    }
    objects = $sceneObjects
  }
}

try {
  # A prior manifest is the resumable plan. ForceRescan is explicit because a
  # new plan changes the date/position set and must not silently orphan files.
  if ((Test-Path -LiteralPath $manifestPath) -and -not $ForceRescan) {
    $manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
    if (-not ($manifest.PSObject.Properties.Name -contains 'completedAt')) {
      $manifest | Add-Member -NotePropertyName completedAt -NotePropertyValue $null
    }
    $scenes = @($manifest.scenes)
    if ($scenes.Count -eq 0) { throw "Existing manifest '$manifestPath' has no scenes." }
    Write-Host "Reusing existing diverse-atlas plan: $($scenes.Count) scenes, $([math]::Round([double]$manifest.plannedBytes / 1GB, 2)) GiB planned."
  }
  else {
    if ($ForceRescan -and @(Get-ChildItem -LiteralPath $root -Recurse -File -Filter '*.jp2' -ErrorAction SilentlyContinue).Count -gt 0) {
      throw 'ForceRescan requires an empty destination or a new Destination because the old JP2 plan would be orphaned.'
    }
    $existingDates = Get-ExistingDates $ExistingAtlasPath
    $candidatesByTile = @{}
    foreach ($strip in $strips) {
      foreach ($tilePath in $strip.tiles) {
        Write-Host "Screening $($strip.name) / $tilePath ..."
        $dates = @(Get-AvailableDates $tilePath)
        if ($dates.Count -eq 0) { throw "No source dates found for $tilePath." }
        $accepted = [System.Collections.Generic.List[object]]::new()
        $examined = 0
        foreach ($date in $dates) {
          if ($accepted.Count -ge $MaxCandidatesPerTile) { break }
          $dateText = $date.ToString('yyyy-MM-dd')
          if ($existingDates.Contains($dateText)) { continue }
          $examined++
          $candidate = Get-TileCandidate $strip.name $tilePath $date
          if ($null -ne $candidate) {
            $accepted.Add($candidate)
            Write-Host ("  accepted {0}: {1:N2} GiB, center {2:N3},{3:N3}" -f $dateText, ($candidate.bytes / 1GB), $candidate.tile.center.latitude, $candidate.tile.center.longitude)
          }
          if ($examined % 50 -eq 0) { Write-Host "  screened $examined dates; accepted $($accepted.Count)" }
        }
        if ($accepted.Count -eq 0) { throw "No quality-qualified scenes found for $tilePath." }
        $candidatesByTile[$tilePath] = @($accepted | Sort-Object acquired)
        Write-Host "  $tilePath`: $($accepted.Count) quality-qualified scenes from $examined screened dates."
      }
    }

    $usedDates = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($date in $existingDates) { [void]$usedDates.Add($date) }
    $selectedCandidates = [System.Collections.Generic.List[object]]::new()
    $tileCursors = @{}
    $stripCursors = @{}
    foreach ($strip in $strips) {
      $stripCursors[$strip.name] = 0
      foreach ($tilePath in $strip.tiles) { $tileCursors[$tilePath] = 0 }
    }
    $plannedBytes = [int64]0

    # Seed every distinct position before filling the byte target. This keeps
    # a small test target from accidentally bypassing spatial diversity.
    foreach ($strip in $strips) {
      foreach ($tilePath in $strip.tiles) {
        $list = @($candidatesByTile[$tilePath])
        while ($tileCursors[$tilePath] -lt $list.Count) {
          $candidate = $list[$tileCursors[$tilePath]]
          $tileCursors[$tilePath]++
          if ($usedDates.Contains($candidate.acquired)) { continue }
          [void]$usedDates.Add($candidate.acquired)
          $selectedCandidates.Add($candidate)
          $plannedBytes += [int64]$candidate.bytes
          break
        }
      }
    }

    while ($plannedBytes -lt $targetBytes) {
      $madeProgress = $false
      foreach ($strip in $strips) {
        $tiles = @($strip.tiles)
        $start = [int]$stripCursors[$strip.name]
        for ($offset = 0; $offset -lt $tiles.Count; $offset++) {
          $tilePath = $tiles[($start + $offset) % $tiles.Count]
          $list = @($candidatesByTile[$tilePath])
          while ($tileCursors[$tilePath] -lt $list.Count) {
            $candidate = $list[$tileCursors[$tilePath]]
            $tileCursors[$tilePath]++
            if ($usedDates.Contains($candidate.acquired)) { continue }
            [void]$usedDates.Add($candidate.acquired)
            $selectedCandidates.Add($candidate)
            $plannedBytes += [int64]$candidate.bytes
            $madeProgress = $true
            break
          }
          if ($madeProgress) { break }
        }
        $stripCursors[$strip.name] = ($start + 1) % $tiles.Count
        if ($plannedBytes -ge $targetBytes) { break }
      }
      if (-not $madeProgress) {
        throw "Only $([math]::Round($plannedBytes / 1GB, 2)) GiB could be selected; no more unique-date quality-qualified scenes remain."
      }
    }

    $scenes = @($selectedCandidates | ForEach-Object { New-ManifestScene $_ })
    $distinctCenters = @{}
    foreach ($scene in $scenes) { $distinctCenters[$scene.tile.tileId] = $scene.center }
    $tileIds = @($distinctCenters.Keys)
    $minDistance = [double]::PositiveInfinity
    $maxDistance = 0.0
    for ($i = 0; $i -lt $tileIds.Count; $i++) {
      for ($j = $i + 1; $j -lt $tileIds.Count; $j++) {
        $distance = Get-HaversineKm $distinctCenters[$tileIds[$i]] $distinctCenters[$tileIds[$j]]
        $minDistance = [math]::Min($minDistance, $distance)
        $maxDistance = [math]::Max($maxDistance, $distance)
      }
    }
    if ($tileIds.Count -lt 8 -or $minDistance -gt 600 -or $maxDistance -lt 3000) {
      throw "Spatial diversity check failed: tileCount=$($tileIds.Count), nearestDifferentTileKm=$([math]::Round($minDistance, 1)), farthestTileKm=$([math]::Round($maxDistance, 1))."
    }
    $dateCount = @($scenes | ForEach-Object { $_.acquired } | Sort-Object -Unique).Count
    if ($dateCount -ne $scenes.Count) { throw "Date uniqueness check failed: $dateCount unique dates for $($scenes.Count) scenes." }
    foreach ($strip in $strips) {
      if (@($scenes | Where-Object { $_.strip -eq $strip.name }).Count -eq 0) { throw "Strip diversity check failed: no scenes selected for $($strip.name)." }
    }

    $manifest = [ordered]@{
      schemaVersion = 1
      title = 'River Sentinel-2 diverse temporal/spatial RGB atlas'
      source = 'Sentinel-2 Level-1C public S3 archive'
      layout = 'sceneId/tileId/B04.jp2, B03.jp2, B02.jp2'
      targetBytes = $targetBytes
      plannedBytes = $plannedBytes
      downloadedBytes = 0
      downloadState = 'planned'
      generatedAt = (Get-Date).ToUniversalTime().ToString('o')
      completedAt = $null
      selection = [ordered]@{
        startDate = $StartDate.ToString('yyyy-MM-dd')
        endDate = $EndDate.ToString('yyyy-MM-dd')
        oneTilePerScene = $true
        uniqueAcquisitionDate = $true
        excludedExistingDates = $ExistingAtlasPath
        maxCloudPercentage = $MaxCloudPercentage
        minCoveragePercentage = $MinCoveragePercentage
        minMeanLuma = $MinMeanLuma
        minP10Luma = $MinP10Luma
        maxDarkPixelPercentage = $MaxDarkPixelPercentage
        nearestDifferentTileKm = [math]::Round($minDistance, 3)
        farthestTileKm = [math]::Round($maxDistance, 3)
      }
      strips = @($strips | ForEach-Object { [ordered]@{ name = $_.name; tiles = $_.tiles; relationship = 'near-pair' } })
      scenes = $scenes
    }
    Write-JsonAtomic $manifest $manifestPath
    Write-Host "Wrote plan: $manifestPath ($($scenes.Count) scenes, $([math]::Round([double]$manifest.plannedBytes / 1GB, 2)) GiB planned)."

    foreach ($candidate in $selectedCandidates) {
      $scene = $scenes | Where-Object { $_.sceneId -eq $candidate.sceneId } | Select-Object -First 1
      Write-SelectedEvidence $scene $candidate
    }
  }

  if ($PlanOnly) {
    Write-Host 'PlanOnly requested; no JP2 files were downloaded.'
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
    if ($Job.process.ExitCode -ne 0) {
      throw "Download failed ($($Job.process.ExitCode)) for $($Job.object.url): $stderr"
    }
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
      foreach ($job in @($active)) {
        if ($job.process.HasExited) { $finished = $job; break }
      }
      if ($null -eq $finished) {
        Start-Sleep -Milliseconds 500
        continue
      }
      Complete-CurlDownload $finished
      [void]$active.Remove($finished)
    }
  }

  try {
    foreach ($scene in @($manifest.scenes)) {
      foreach ($object in @($scene.objects)) {
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
      foreach ($job in @($active)) {
        if ($job.process.HasExited) { $finished = $job; break }
      }
      if ($null -eq $finished) {
        Start-Sleep -Milliseconds 500
        continue
      }
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
  foreach ($scene in @($manifest.scenes)) {
    foreach ($object in @($scene.objects)) {
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
  foreach ($path in $actualJp2) {
    if (-not $objectPaths.Contains($path)) { throw "Unexpected JP2 file in corpus: $path" }
  }

  $manifest.downloadedBytes = $downloadedBytes
  $manifest.downloadState = 'complete'
  $manifest.completedAt = (Get-Date).ToUniversalTime().ToString('o')
  Write-JsonAtomic $manifest $manifestPath
  Add-Content -LiteralPath $logPath -Encoding utf8 -Value "$(Get-Date -Format o) completed $downloadedBytes bytes across $($manifest.scenes.Count) scenes"
  Write-Host "Complete: $($manifest.scenes.Count) scenes, $([math]::Round($downloadedBytes / 1GB, 2)) GiB, $($objectPaths.Count) JP2 objects."
}
finally {
  if ($null -ne $httpClient) { $httpClient.Dispose() }
}
