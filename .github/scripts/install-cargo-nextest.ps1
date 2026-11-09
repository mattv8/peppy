[CmdletBinding()]
param(
  [Parameter(Position = 0)]
  [string]$BinDir
)

$ErrorActionPreference = 'Stop'
$NextestVersion = '0.9.148'
$NextestReleaseBaseUrl = "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$NextestVersion"

function Get-NextestRelease {
  param([Parameter(Mandatory)][string]$Architecture)

  switch ($Architecture.ToUpperInvariant()) {
    'AMD64' {
      return [pscustomobject]@{
        Triple = 'x86_64-pc-windows-msvc'
        Sha512 = '7eb3bd3a6ccbf74b2cdba604cf23e08c9801f15f7ecbf65f108f4d1f2e81207305c6a82d459aafa75b0c68771e9c9113ae9110d68951ec52a09f7125ce432158'
        Sha256 = '7a931b4a9123fe5e641dd0f2b73d897f64bcf77bb06f080db17cb434b7eac782'
      }
    }
    'ARM64' {
      return [pscustomobject]@{
        Triple = 'aarch64-pc-windows-msvc'
        Sha512 = '6b9e1d74cabdab146325b11d19ee0320123f17f7d1b2fab1be657b85b86effb895c2561f219755d6b1fd9b2e52880115f53af484406baaa52e2ecc380967384f'
        Sha256 = '5749f1354c38dfe5068c788ee5a224072edbb7233dd04557b1e7fc9b7b7c3e0a'
      }
    }
    default { throw "unsupported Windows architecture: $Architecture" }
  }
}

function Assert-ArchiveChecksum {
  param(
    [Parameter(Mandatory)][string]$Archive,
    [Parameter(Mandatory)][string]$ExpectedSha512
  )

  $actual = (Get-FileHash -Algorithm SHA512 -LiteralPath $Archive).Hash.ToLowerInvariant()
  if ($actual -ne $ExpectedSha512) {
    throw 'cargo-nextest checksum mismatch'
  }
}

function Assert-PublishedChecksum {
  param(
    [Parameter(Mandatory)][string]$ChecksumFile,
    [Parameter(Mandatory)][string]$ArchiveName,
    [Parameter(Mandatory)][string]$ExpectedSha256
  )

  $expectedLine = "$ExpectedSha256 *$ArchiveName"
  if (-not (Select-String -LiteralPath $ChecksumFile -SimpleMatch -Quiet -Pattern $expectedLine)) {
    throw 'cargo-nextest published checksum mismatch'
  }
}

function Install-CargoNextest {
  param(
    [Parameter(Mandatory)][string]$Destination,
    [Parameter(Mandatory)][pscustomobject]$Release,
    [Parameter(Mandatory)][string]$ReleaseBaseUrl,
    [scriptblock]$Download = {
      param([string]$Uri, [string]$OutputPath)
      Invoke-WebRequest -Uri $Uri -OutFile $OutputPath
    }
  )

  $existing = Join-Path $Destination 'cargo-nextest.exe'
  if ((Test-Path -LiteralPath $existing) -and ((& $existing --version).Split()[1] -eq $NextestVersion)) {
    return
  }

  New-Item -ItemType Directory -Force -Path $Destination | Out-Null
  $temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
  New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
  try {
    $archiveName = "cargo-nextest-$NextestVersion-$($Release.Triple).zip"
    $archive = Join-Path $temporaryDirectory $archiveName
    $checksumName = "cargo-nextest-$NextestVersion-$($Release.Triple).sha256"
    $checksumFile = Join-Path $temporaryDirectory $checksumName
    & $Download "$ReleaseBaseUrl/$checksumName" $checksumFile
    Assert-PublishedChecksum -ChecksumFile $checksumFile -ArchiveName $archiveName -ExpectedSha256 $Release.Sha256
    & $Download "$ReleaseBaseUrl/$archiveName" $archive
    Assert-ArchiveChecksum -Archive $archive -ExpectedSha512 $Release.Sha512
    Expand-Archive -LiteralPath $archive -DestinationPath $temporaryDirectory -Force
    $executable = Join-Path $temporaryDirectory 'cargo-nextest.exe'
    if (-not (Test-Path -LiteralPath $executable)) {
      throw 'cargo-nextest archive did not contain cargo-nextest.exe'
    }
    Copy-Item -LiteralPath $executable -Destination $existing -Force
  } finally {
    Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force -ErrorAction SilentlyContinue
  }
}

if ($MyInvocation.InvocationName -ne '.') {
  if ([string]::IsNullOrWhiteSpace($BinDir)) {
    throw "usage: $($MyInvocation.MyCommand.Name) <bin-dir>"
  }
  $release = Get-NextestRelease -Architecture $env:PROCESSOR_ARCHITECTURE
  Install-CargoNextest -Destination $BinDir -Release $release -ReleaseBaseUrl $NextestReleaseBaseUrl
}
