<#
.SYNOPSIS
  Builds a release ChimeraIRC and packages it: a portable ZIP and an MSI installer (per-user, no admin rights).

.DESCRIPTION
  Output goes to dist\ChimeraIRC-<version>-x64.msi and dist\ChimeraIRC-<version>-x64-portable.zip.

  Tools: cargo, and the WiX Toolset 5 command line (wix.dll) with its UI extension, expected under tools\ (see
  -WixDir). Nothing is installed system-wide.

  Signing: pass -Sign to sign the exe and the MSI with a code-signing certificate from a PFX file. The path comes
  from -Pfx or $env:CHIMERA_SIGN_PFX and the password only from $env:CHIMERA_SIGN_PASSWORD (it is never written
  anywhere). Without -Sign the files are unsigned, and Windows SmartScreen will warn about them.

.EXAMPLE
  .\packaging\build-installer.ps1
  $env:CHIMERA_SIGN_PASSWORD = Read-Host   # then:
  .\packaging\build-installer.ps1 -Sign -Pfx C:\keys\mycert.pfx
#>
[CmdletBinding()]
param(
  [switch]$Sign,
  [string]$Pfx = $env:CHIMERA_SIGN_PFX,
  [string]$TimestampUrl = "http://timestamp.digicert.com",
  [string]$WixDir = "$PSScriptRoot\..\tools",
  [switch]$SkipMsi
)
$ErrorActionPreference = "Stop"
$root = (Resolve-Path "$PSScriptRoot\..").Path
Set-Location $root

# ---- version: the workspace version in Cargo.toml
$cargo = Get-Content "$root\Cargo.toml" -Raw
if ($cargo -notmatch '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"') { throw "no version found in Cargo.toml" }
$version = $Matches[1]
$dist = "$root\dist"
New-Item -ItemType Directory -Force $dist | Out-Null
Write-Host "ChimeraIRC $version"

# ---- build (the file version gets a build number from the commit count, so each build upgrades the one before it)
$env:CHIMERA_BUILD = (git rev-list --count HEAD 2>$null)
cargo build --release -p chimerairc
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$exe = "$root\target\release\chimerairc.exe"
if (-not (Test-Path $exe)) { throw "$exe was not built" }

# ---- signing helper (Authenticode through PowerShell: no SDK needed)
function Sign-File([string]$path) {
  if (-not $Sign) { return }
  if (-not $Pfx -or -not (Test-Path $Pfx)) { throw "-Sign needs a PFX file: pass -Pfx or set CHIMERA_SIGN_PFX" }
  if (-not $env:CHIMERA_SIGN_PASSWORD) { throw "set `$env:CHIMERA_SIGN_PASSWORD to the PFX password (it is not stored anywhere)" }
  $pw = ConvertTo-SecureString $env:CHIMERA_SIGN_PASSWORD -AsPlainText -Force
  $cert = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($Pfx, $pw)
  $args2 = @{ FilePath = $path; Certificate = $cert; HashAlgorithm = "SHA256" }
  if ($TimestampUrl) { $args2.TimestampServer = $TimestampUrl }   # the timestamp keeps the signature valid after the certificate expires
  $r = Set-AuthenticodeSignature @args2
  if ($r.Status -ne "Valid" -and $r.Status -ne "UnknownError") { Write-Warning "signing $path : $($r.Status) $($r.StatusMessage)" }
  Write-Host "signed $(Split-Path $path -Leaf): $($r.Status)"
}

# ---- stage the files the packages hold (the exe is signed before it goes in)
$stage = "$dist\stage"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item $exe "$stage\chimerairc.exe"
Sign-File "$stage\chimerairc.exe"
Copy-Item "$root\LICENSE" "$stage\LICENSE.txt"
@"
ChimeraIRC $version

An IRC client for Windows, a Rust rewrite in the spirit of HydraIRC.
Settings are kept in %AppData%\ChimeraIRC\settings.ini.

Licence: GPL-3.0-or-later (see LICENSE.txt).
"@ | Set-Content "$stage\README.txt" -Encoding UTF8

# ---- portable ZIP
$zip = "$dist\ChimeraIRC-$version-x64-portable.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\chimerairc.exe", "$stage\LICENSE.txt", "$stage\README.txt" -DestinationPath $zip
Write-Host "wrote $zip"

if ($SkipMsi) { return }

# ---- MSI
$wixDll = Get-ChildItem "$WixDir\wix" -Recurse -Filter wix.dll -ErrorAction SilentlyContinue | Select-Object -First 1
$uiExt = Get-ChildItem "$WixDir\wixui" -Recurse -Filter WixToolset.UI.wixext.dll -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $wixDll -or -not $uiExt) { throw "the WiX Toolset was not found under $WixDir (tools\wix and tools\wixui); see packaging\README.md" }
$env:DOTNET_ROLL_FORWARD = "Major"   # wix.dll targets .NET 6; newer runtimes are fine

# the licence page wants RTF: wrap the plain text
$text = (Get-Content "$root\LICENSE" -Raw) -replace '\\', '\\\\' -replace '\{', '\{' -replace '\}', '\}'
$rtfBody = ($text -split "`r?`n" | ForEach-Object { $_ + '\par' }) -join "`r`n"
$rtf = "{\rtf1\ansi\deff0{\fonttbl{\f0\fmodern Consolas;}}\f0\fs18`r`n$rtfBody`r`n}"
Set-Content "$stage\License.rtf" $rtf -Encoding ASCII

$msi = "$dist\ChimeraIRC-$version-x64.msi"
Remove-Item $msi -ErrorAction SilentlyContinue
& dotnet $wixDll.FullName build "$root\packaging\ChimeraIRC.wxs" `
  -arch x64 -o $msi `
  -ext $uiExt.FullName `
  -d "Version=$version" -d "ExePath=$stage\chimerairc.exe" -d "LicenseRtf=$stage\License.rtf" `
  -d "LicenseTxt=$stage\LICENSE.txt" -d "ReadmeTxt=$stage\README.txt" `
  -d "Icon=$root\crates\chimerairc-gui\assets\chimerairc.ico"
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }
Sign-File $msi
Write-Host "wrote $msi"
Get-ChildItem $dist -File | Select-Object Name, @{n="KB"; e={[int]($_.Length / 1KB)}}
