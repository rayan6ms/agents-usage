$ErrorActionPreference = "Stop"

$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$VersionLine = Select-String -Path (Join-Path $Root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
$Version = $VersionLine.Matches[0].Groups[1].Value
$Binary = if ($args.Count -gt 0) { (Resolve-Path $args[0]).Path } else { Join-Path $Root "target\release\agents-usage.exe" }
$Dist = Join-Path $Root "dist\packages"
$Stage = Join-Path $Root "dist\.windows-package-work\Agents Usage"
$Archive = Join-Path $Dist "Agents_Usage-$Version-windows-x86_64.zip"

if (-not (Test-Path $Binary -PathType Leaf)) { throw "Missing executable: $Binary" }
Remove-Item (Split-Path $Stage) -Recurse -Force -ErrorAction SilentlyContinue
New-Item $Stage -ItemType Directory -Force | Out-Null
New-Item $Dist -ItemType Directory -Force | Out-Null
Copy-Item $Binary (Join-Path $Stage "agents-usage.exe")
Copy-Item (Join-Path $Root "README.md") $Stage
Copy-Item (Join-Path $Root "LICENSE") $Stage
Remove-Item $Archive -Force -ErrorAction SilentlyContinue
Compress-Archive -Path "$Stage\*" -DestinationPath $Archive -CompressionLevel Optimal
if ((Get-Item $Archive).Length -le 0) { throw "Windows package is empty" }
$Verify = Join-Path $Root "dist\.windows-package-verify"
Remove-Item $Verify -Recurse -Force -ErrorAction SilentlyContinue
Expand-Archive $Archive $Verify
if (-not (Test-Path (Join-Path $Verify "agents-usage.exe") -PathType Leaf)) { throw "Windows package is missing agents-usage.exe" }
if (-not (Test-Path (Join-Path $Verify "LICENSE") -PathType Leaf)) { throw "Windows package is missing LICENSE" }
$Header = [System.IO.File]::ReadAllBytes((Join-Path $Verify "agents-usage.exe"))[0..1]
if ($Header[0] -ne 0x4d -or $Header[1] -ne 0x5a) { throw "Packaged executable is not a PE file" }
Remove-Item $Verify -Recurse -Force
Write-Host "Windows portable ZIP is ready at $Archive (unsigned)."
