param([Parameter(Mandatory = $true)][string]$Artifact)

$thumbprint = $env:PEPPY_WINDOWS_CERT_THUMBPRINT
if ([string]::IsNullOrWhiteSpace($thumbprint)) { throw 'PEPPY_WINDOWS_CERT_THUMBPRINT is required' }
$signtoolCandidates = Get-ChildItem -Path (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin') -Filter signtool.exe -Recurse -File -ErrorAction SilentlyContinue |
  Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
  Sort-Object FullName -Descending
if ($signtoolCandidates.Count -eq 0) { throw 'Windows Kits x64 signtool.exe was not found' }
$signtool = $signtoolCandidates[0].FullName
& $signtool sign /sha1 $thumbprint /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 $Artifact
if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed for $Artifact" }
& $signtool verify /pa /all $Artifact
if ($LASTEXITCODE -ne 0) { throw "Authenticode verification failed for $Artifact" }
