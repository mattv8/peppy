$ErrorActionPreference = 'Stop'
$failures = [System.Collections.Generic.List[string]]::new()

function Assert-Equal {
  param($Expected, $Actual, [string]$Message)
  if ($Expected -ne $Actual) { $script:failures.Add("$Message (expected '$Expected', got '$Actual')") }
}

. (Join-Path $PSScriptRoot 'install-cargo-nextest.ps1')

$release = Get-NextestRelease -Architecture 'AMD64'
Assert-Equal 'x86_64-pc-windows-msvc' $release.Triple 'AMD64 must select the x64 release'
Assert-Equal 128 $release.Sha512.Length 'The x64 SHA512 must be pinned'
Assert-Equal 64 $release.Sha256.Length 'The x64 official release checksum must be pinned'

$armRelease = Get-NextestRelease -Architecture 'ARM64'
Assert-Equal 'aarch64-pc-windows-msvc' $armRelease.Triple 'ARM64 must select the native release'

try {
  Get-NextestRelease -Architecture 'x86' | Out-Null
  $failures.Add('Unsupported architectures must fail')
} catch {}

$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
try {
  $sourceDirectory = Join-Path $temporaryDirectory 'source'
  $destination = Join-Path $temporaryDirectory 'destination'
  New-Item -ItemType Directory -Path $sourceDirectory | Out-Null
  $sourceExecutable = Join-Path $sourceDirectory 'cargo-nextest.exe'
  Set-Content -NoNewline -LiteralPath $sourceExecutable -Value 'stub executable'
  $archive = Join-Path $temporaryDirectory 'cargo-nextest.zip'
  Compress-Archive -LiteralPath $sourceExecutable -DestinationPath $archive
  $sha512 = (Get-FileHash -Algorithm SHA512 -LiteralPath $archive).Hash.ToLowerInvariant()
  $sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
  $stubRelease = [pscustomobject]@{ Triple = 'stub'; Sha512 = $sha512; Sha256 = $sha256 }
  $archiveName = 'cargo-nextest-0.9.148-stub.zip'
  $checksumName = 'cargo-nextest-0.9.148-stub.sha256'
  $download = {
    param($Uri, $OutputPath)
    if ($Uri -eq "https://stub.invalid/$checksumName") {
      Set-Content -NoNewline -LiteralPath $OutputPath -Value @"
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa *cargo-nextest-0.9.148-stub.tar.gz
$sha256 *$archiveName
"@
    } elseif ($Uri -eq "https://stub.invalid/$archiveName") {
      Copy-Item -LiteralPath $archive -Destination $OutputPath
    } else {
      throw "unexpected download URL: $Uri"
    }
  }.GetNewClosure()
  Install-CargoNextest -Destination $destination -Release $stubRelease -ReleaseBaseUrl 'https://stub.invalid' -Download $download
  Assert-Equal 'stub executable' (Get-Content -Raw (Join-Path $destination 'cargo-nextest.exe')) 'The installer must extract the verified executable'

  $badRelease = [pscustomobject]@{ Triple = 'stub'; Sha512 = ('0' * 128); Sha256 = $sha256 }
  try {
    Install-CargoNextest -Destination (Join-Path $temporaryDirectory 'bad-destination') -Release $badRelease -ReleaseBaseUrl 'https://stub.invalid' -Download $download
    $failures.Add('A mismatched archive checksum must fail')
  } catch {}
} finally {
  Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) { $failures | ForEach-Object { [Console]::Error.WriteLine($_) }; exit 1 }
Write-Output 'install-cargo-nextest.Tests.ps1: passed'
