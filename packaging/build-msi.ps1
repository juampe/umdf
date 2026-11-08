param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$BinDir,
    [Parameter(Mandatory = $true)][string]$OutDir
)

$ErrorActionPreference = "Stop"

$wixRoot = Get-ChildItem "${env:ProgramFiles(x86)}\WiX Toolset*" -Directory -ErrorAction SilentlyContinue |
    Select-Object -First 1
if (-not $wixRoot) {
    throw "WiX Toolset not found. Install it with: choco install wixtoolset -y"
}
$candle = Join-Path $wixRoot.FullName "bin\candle.exe"
$light = Join-Path $wixRoot.FullName "bin\light.exe"

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$wixobj = Join-Path $env:RUNNER_TEMP "umdf.wixobj"
$license = Join-Path $BinDir "LICENSE.rtf"
$licenseArg = "-dWixUILicenseRtf=$license"
$licenseArgAlias = "-dlicenseRtf=$license"

& $candle -nologo -ext WixUIExtension -dVersion="$Version" -dBinDir="$BinDir" `
    $licenseArg $licenseArgAlias packaging\umdf.wxs -out $wixobj
if ($LASTEXITCODE -ne 0) { throw "candle failed" }

& $light -nologo -ext WixUIExtension $wixobj `
    -out (Join-Path $OutDir "umdf_${Version}_x86_64.msi")
if ($LASTEXITCODE -ne 0) { throw "light failed" }

Write-Host "Created $(Join-Path $OutDir "umdf_${Version}_x86_64.msi")"
