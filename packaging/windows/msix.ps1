# Packs a release copiale.exe into an unsigned .msix. Windows only; needs the
# Windows SDK.
#
#   msix.ps1 -Exe <copiale.exe> -Version <X.Y.Z> -Publisher <subject> -Out <file>
#
# Publisher must match the signing certificate's subject exactly.
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$Publisher,
    [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = "Stop"

$bin = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\10.*\x64\makeappx.exe" |
    Sort-Object { [version]$_.Directory.Parent.Name } |
    Select-Object -Last 1 -ExpandProperty Directory

$work = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
$layout = Join-Path $work layout
New-Item -ItemType Directory $layout | Out-Null
Copy-Item $Exe (Join-Path $layout copiale.exe)
Copy-Item (Join-Path $PSScriptRoot assets) $layout -Recurse

$manifest = [xml]::new()
$manifest.Load((Join-Path $PSScriptRoot AppxManifest.xml))
$manifest.Package.Identity.Publisher = $Publisher
$manifest.Package.Identity.Version = "$Version.0"
$manifest.Save((Join-Path $layout AppxManifest.xml))

# The index of icon sizes, which the shell picks from.
$config = Join-Path $work priconfig.xml
& "$bin\makepri.exe" createconfig /cf $config /dq en-us /pv 10.0.0 /o
if ($LASTEXITCODE -ne 0) { throw "makepri createconfig failed" }
& "$bin\makepri.exe" new /pr $layout /cf $config /mn (Join-Path $layout AppxManifest.xml) /of (Join-Path $layout resources.pri) /o
if ($LASTEXITCODE -ne 0) { throw "makepri new failed" }

New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
& "$bin\makeappx.exe" pack /o /d $layout /p $Out
if ($LASTEXITCODE -ne 0) { throw "makeappx failed" }
Remove-Item $work -Recurse
