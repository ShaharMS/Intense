param([switch]$Gpu, [switch]$SkipLoad)
$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
if (-not $SkipLoad) {
    foreach ($archive in @('river-backend-linux-amd64.tar.gz', 'river-ui-linux-amd64.tar.gz')) {
        $expected = ((Select-String -LiteralPath './SHA256SUMS' -SimpleMatch $archive).Line -split '\s+')[0]
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) { throw "Checksum failed: $archive" }
        docker load --input $archive
        if ($LASTEXITCODE -ne 0) { throw "Cannot load $archive" }
    }
}
if (-not (Test-Path -LiteralPath './.env')) {
    Copy-Item -LiteralPath './.env.example' -Destination './.env'
}
$arguments = @('compose','-p','river-portable','-f','compose.yaml')
if ($Gpu) { $arguments += @('-f','compose.gpu.yaml') }
$arguments += @('up','-d','--wait','--wait-timeout','240')
docker @arguments
if ($LASTEXITCODE -ne 0) { throw 'Deployment failed; inspect docker compose logs' }
Write-Output 'River started. Default UI: http://127.0.0.1:18765/; Swagger: http://127.0.0.1:18080/docs/'
