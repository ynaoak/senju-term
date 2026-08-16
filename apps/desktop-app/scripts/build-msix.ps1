<#
.SYNOPSIS
  Packs the built Windows binary into an MSIX for Microsoft Store submission.

.DESCRIPTION
  The Store cannot take the NSIS/MSI installers this repo already builds, so the
  Store artifact is assembled separately: a package layout containing the
  release binary, the Store tile assets and a rendered AppxManifest.xml, packed
  with the Windows SDK's makeappx.exe.

  The package is left UNSIGNED on purpose. Partner Center re-signs every
  submission with the publisher's Store certificate, and signing here with any
  other certificate would only produce a package the Store rejects. To sideload
  the output for local testing you have to sign it yourself first — see
  docs/release-and-distribution.md §5.

  The whole frontend is embedded in the executable by tauri-build, so the layout
  is just the binary plus assets; there is no separate web-asset tree to copy.

.EXAMPLE
  pwsh scripts/build-msix.ps1 -Exe target/release/senju-term.exe `
      -Version 0.1.0 -IdentityName 'Publisher.SenjuTerm' `
      -Publisher 'CN=…' -PublisherDisplayName 'Your Name'
#>
[CmdletBinding()]
param(
  # The compiled release binary to package.
  [Parameter(Mandatory = $true)][string]$Exe,
  # App version as `major.minor.patch` (from tauri.conf.json).
  [Parameter(Mandatory = $true)][string]$Version,
  # Partner Center "Package/Identity/Name".
  [Parameter(Mandatory = $true)][string]$IdentityName,
  # Partner Center "Package/Identity/Publisher" (a full `CN=…` subject).
  [Parameter(Mandatory = $true)][string]$Publisher,
  # Partner Center "Package/Properties/PublisherDisplayName".
  [Parameter(Mandatory = $true)][string]$PublisherDisplayName,
  # Where the .msix is written.
  [string]$OutDir = 'target/msstore'
)

$ErrorActionPreference = 'Stop'

# `utf8NoBOM` below only exists in PowerShell 7+. Windows PowerShell 5.1 would
# instead write a BOM, and makeappx rejects a manifest that starts with one —
# with an error that says nothing about encoding. Fail with the real reason.
if ($PSVersionTable.PSVersion.Major -lt 7) {
  throw "PowerShell 7+ required (run with `pwsh`); found $($PSVersionTable.PSVersion)"
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$msstoreDir = Join-Path $repoRoot 'src-tauri/msstore'
$templatePath = Join-Path $msstoreDir 'AppxManifest.xml'
$assetsDir = Join-Path $msstoreDir 'assets'

if (-not (Test-Path -LiteralPath $Exe)) {
  throw "Executable not found: $Exe (build it first with `cargo tauri build`)"
}

# MSIX versions are always four parts. The Store additionally reserves the
# revision field for its own use and rejects a package whose revision is not 0,
# so it is pinned rather than derived from anything.
if ($Version -notmatch '^\d+\.\d+\.\d+$') {
  throw "Version must be major.minor.patch, got '$Version'"
}
$msixVersion = "$Version.0"

# makeappx.exe lives in a versioned Windows SDK directory; take the newest.
$makeappx = Get-ChildItem -Path 'C:\Program Files (x86)\Windows Kits\10\bin' `
    -Filter 'makeappx.exe' -Recurse -ErrorAction SilentlyContinue |
  Where-Object { $_.FullName -match '\\x64\\' } |
  Sort-Object -Property FullName -Descending |
  Select-Object -First 1
if (-not $makeappx) {
  throw 'makeappx.exe not found. Install the Windows 10/11 SDK (App Certification / packaging tools).'
}

$layout = Join-Path $OutDir 'layout'
if (Test-Path -LiteralPath $layout) { Remove-Item -LiteralPath $layout -Recurse -Force }
New-Item -ItemType Directory -Path $layout -Force | Out-Null

# The manifest names the executable `senju-term.exe`, so copy it under that name
# regardless of what the caller passed in.
Copy-Item -LiteralPath $Exe -Destination (Join-Path $layout 'senju-term.exe') -Force
Copy-Item -LiteralPath $assetsDir -Destination (Join-Path $layout 'assets') -Recurse -Force

$manifest = Get-Content -LiteralPath $templatePath -Raw
$manifest = $manifest.
  Replace('__IDENTITY_NAME__', $IdentityName).
  Replace('__PUBLISHER__', $Publisher).
  Replace('__PUBLISHER_DISPLAY_NAME__', $PublisherDisplayName).
  Replace('__VERSION__', $msixVersion)
if ($manifest -match '__[A-Z_]+__') {
  throw "AppxManifest still contains unsubstituted tokens: $($Matches[0])"
}
# -Encoding utf8NoBOM: makeappx rejects a manifest that starts with a BOM.
Set-Content -LiteralPath (Join-Path $layout 'AppxManifest.xml') -Value $manifest -Encoding utf8NoBOM

$msix = Join-Path $OutDir "senju-term_${Version}_x64.msix"
& $makeappx.FullName pack /d $layout /p $msix /o
if ($LASTEXITCODE -ne 0) { throw "makeappx failed with exit code $LASTEXITCODE" }

Write-Host "MSIX written to $msix (unsigned — Partner Center signs it on submission)"
