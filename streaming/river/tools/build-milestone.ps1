param([Parameter(Mandatory=$true)][string]$Milestone,[string]$SourceRoot,[switch]$TrialOnly)
$ErrorActionPreference='Stop'
# All builds are candidates until separately qualified; channels share one image.
$arguments=@((Join-Path $PSScriptRoot 'build-release.py'),$Milestone)
if ($SourceRoot) {$arguments+=@('--source-root',$SourceRoot)}
python @arguments
if ($LASTEXITCODE) {throw "River build failed; see target/builds/$Milestone/build.log"}
