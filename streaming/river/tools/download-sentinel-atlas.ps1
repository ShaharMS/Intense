param(
  [int]$TargetGiB = 30,
  [string]$Destination = (Join-Path $PSScriptRoot '..\corpus\cache\sentinel-atlas'),
  [string]$LogPath = (Join-Path $PSScriptRoot '..\corpus\cache\sentinel-atlas-download.log')
)

$ErrorActionPreference = 'Stop'
$targetBytes = [int64]$TargetGiB * 1GB
$root = [System.IO.Path]::GetFullPath($Destination)
New-Item -ItemType Directory -Force -Path $root | Out-Null

# A single Sentinel-2A acquisition covers this contiguous four-tile west-to-east
# strip.  Each 10 m component is an independently decodable JPEG 2000 file.
$squares = @('DG', 'EG', 'FG', 'GG')
$bands = @('B04', 'B03', 'B02') # RGB source order: red, green, blue
$baseUrl = 'https://sentinel-s2-l1c.s3.amazonaws.com/tiles/10/S'
$selected = @()
$bytesPlanned = [int64]0

function Get-ContentLength([string]$url) {
  $headers = curl.exe --head --location --silent --show-error $url
  if ($LASTEXITCODE -ne 0) { return $null }
  $match = $headers | Select-String -Pattern '^Content-Length:\s*(\d+)$' -CaseSensitive:$false | Select-Object -Last 1
  if ($null -eq $match) { return $null }
  return [int64]$match.Matches[0].Groups[1].Value
}

for ($date = [datetime]'2015-12-07'; $bytesPlanned -lt $targetBytes -and $date -lt [datetime]'2026-01-01'; $date = $date.AddDays(10)) {
  $datePath = $date.ToString('yyyy/M/d')
  $objects = @()
  $complete = $true
  foreach ($square in $squares) {
    foreach ($band in $bands) {
      $url = "$baseUrl/$square/$datePath/0/$band.jp2"
      $length = Get-ContentLength $url
      if ($null -eq $length) { $complete = $false; break }
      $objects += [pscustomobject]@{ square = $square; band = $band; url = $url; bytes = $length }
    }
    if (-not $complete) { break }
  }
  if (-not $complete) { continue }
  $sceneBytes = [int64](($objects | Measure-Object -Property bytes -Sum).Sum)
  $selected += [pscustomobject]@{ acquired = $date.ToString('yyyy-MM-dd'); objects = $objects }
  $bytesPlanned += $sceneBytes
}

if ($bytesPlanned -lt $targetBytes) {
  throw "Only planned $bytesPlanned bytes; target is $targetBytes bytes."
}

$manifest = [pscustomobject]@{
  title = 'Sentinel-2A temporal RGB atlas, UTM 10S four-tile strip'
  source = 'Sentinel-2 Level-1C public S3 archive'
  targetBytes = $targetBytes
  plannedBytes = $bytesPlanned
  tilePixels = 10980
  squares = $squares
  rgbBands = $bands
  scenes = $selected
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $root 'atlas.json')

foreach ($scene in $selected) {
  foreach ($object in $scene.objects) {
    $path = Join-Path $root (Join-Path $scene.acquired (Join-Path $object.square "$($object.band).jp2"))
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
    if ((Test-Path -LiteralPath $path) -and (Get-Item -LiteralPath $path).Length -eq $object.bytes) { continue }
    "$(Get-Date -Format o) downloading $($object.url)" | Add-Content -Encoding utf8 $LogPath
    & curl.exe --fail --location --retry 5 --retry-all-errors --continue-at - --output $path $object.url
    if ($LASTEXITCODE -ne 0) { throw "Download failed: $($object.url)" }
    if ((Get-Item -LiteralPath $path).Length -ne $object.bytes) { throw "Size mismatch: $path" }
  }
}

"$(Get-Date -Format o) completed $bytesPlanned bytes across $($selected.Count) scenes" | Add-Content -Encoding utf8 $LogPath
