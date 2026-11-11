$ErrorActionPreference = 'Stop'
$failures = [System.Collections.Generic.List[string]]::new()

function Assert-True {
  param([bool]$Condition, [string]$Message)
  if (-not $Condition) { $script:failures.Add($Message) }
}

$root = (Resolve-Path (Join-Path $PSScriptRoot '../..')).ProviderPath
$desktopScript = Join-Path $root 'infra/dev/windows-desktop.ps1'
. $desktopScript -Action build -RepoPath $root -CargoTargetDir (Join-Path $root '.opencode/test-target')
$desktopSource = Get-Content -LiteralPath $desktopScript -Raw

Assert-True ($desktopSource -match "\[ValidateSet\('0', '1'\)\]") 'Windows quick-reply input must accept only 0 or 1'
Assert-True ($desktopSource -match "notification-quick-reply") 'Windows builds must include the notification quick-reply Cargo feature when requested'

$openTarget = Join-Path ([System.IO.Path]::GetTempPath()) ("peppy-open-" + [guid]::NewGuid())
$openApp = Join-Path $openTarget 'release\peppy-desktop.exe'
New-Item -ItemType Directory -Path (Split-Path $openApp) -Force | Out-Null
New-Item -ItemType File -Path $openApp | Out-Null
$script:openedApp = $null
function global:Start-Process {
  param([string]$FilePath)
  $script:openedApp = $FilePath
}
Invoke-WindowsDesktop -RequestedAction open -Repository $root -TargetDirectory $openTarget -QuickReply '0'
Assert-True ($script:openedApp -eq $openApp) 'Default Windows open must preserve raw executable launch behavior'
Remove-Item function:global:Start-Process

$openRejected = $false
try {
  Invoke-WindowsDesktop -RequestedAction open -Repository $root -TargetDirectory $openTarget -QuickReply '1'
} catch {
  $openRejected = $_.Exception.Message -match 'Install the generated NSIS or MSI bundle' -and $_.Exception.Message -match 'Start menu'
}
Assert-True $openRejected 'Quick-reply Windows open must require installation and Start menu launch for native notification testing'
Remove-Item -LiteralPath $openTarget -Recurse -Force

Assert-True (Test-NodeVersion -Selector '26' -Version 'v26.0.0') 'Major selectors must accept stable Node releases in their major line'
Assert-True (Test-NodeVersion -Selector '26' -Version 'v26.12.3') 'Major selectors must accept later stable patches'
Assert-True (Test-NodeVersion -Selector '26.12' -Version 'v26.12.3') 'Major.minor selectors must accept stable patches in their line'
Assert-True (Test-NodeVersion -Selector '24.21.0' -Version 'v24.21.0') 'Exact selectors must retain exact-version matching'
Assert-True (-not (Test-NodeVersion -Selector '26' -Version 'v260.0.0')) 'Major selectors must not match a longer major'
Assert-True (-not (Test-NodeVersion -Selector '26' -Version 'v26.0.0-rc.1')) 'Node prereleases must not satisfy stable selectors'

$threw = $false
try {
  Invoke-Checked -File $env:ComSpec -Arguments @('/c', 'exit', '3')
} catch {
  $threw = $_.Exception.Message -match 'exit code 3'
}
Assert-True $threw 'Invoke-Checked must throw the nonzero external command exit code'

function global:Get-Command {
  param([string]$Name)
  [pscustomobject]@{ Source = '\\wsl.localhost\Ubuntu\usr\bin\node.exe' }
}
function global:Resolve-Path {
  param([string]$LiteralPath)
  [pscustomobject]@{ ProviderPath = $LiteralPath }
}
$rejectedUnc = $false
try {
  Get-NativeCommand 'node.exe'
} catch {
  $rejectedUnc = $_.Exception.Message -match 'UNC or WSL'
}
Assert-True $rejectedUnc 'Get-NativeCommand must reject a WSL UNC executable path'
Remove-Item function:global:Get-Command
Remove-Item function:global:Resolve-Path

$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("peppy-perl-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporary | Out-Null
$script:perlCandidate = Join-Path $temporary 'perl.cmd'
Set-Content -LiteralPath $script:perlCandidate -Value '@exit /b 0' -NoNewline
function global:Get-Command {
  param([string]$Name)
  if ($Name -eq 'perl.exe') { return [pscustomobject]@{ Source = $script:perlCandidate } }
  Microsoft.PowerShell.Core\Get-Command @PSBoundParameters
}
$selected = Get-NativePerl
Assert-True ($selected -eq (Resolve-Path -LiteralPath $script:perlCandidate).ProviderPath) 'Get-NativePerl must use a native Perl candidate that passes IPC::Cmd'
Remove-Item function:global:Get-Command

$script:pnpmCandidate = Join-Path $temporary 'pnpm.exe'
Set-Content -LiteralPath $script:pnpmCandidate -Value '@exit /b 0' -NoNewline
function global:Get-Command {
  param([string]$Name)
  if ($Name -eq 'pnpm.cmd') { throw 'pnpm.cmd is not installed' }
  if ($Name -eq 'pnpm.exe') { return [pscustomobject]@{ Source = $script:pnpmCandidate } }
  Microsoft.PowerShell.Core\Get-Command @PSBoundParameters
}
$pnpm = Get-NativePnpm
Assert-True ($pnpm -eq (Resolve-Path -LiteralPath $script:pnpmCandidate).ProviderPath) 'Get-NativePnpm must fall back to a native pnpm.exe installation'
Remove-Item function:global:Get-Command

$originalFallback = $script:StrawberryPerlPath
$script:StrawberryPerlPath = Join-Path $temporary 'missing-perl.exe'
function global:Get-Command { throw 'perl.exe is not on PATH' }
$missingFallbackFails = $false
try {
  Get-NativePerl
} catch {
  $missingFallbackFails = $_.Exception.Message -match 'Strawberry Perl'
}
Assert-True $missingFallbackFails 'Get-NativePerl must report a missing configured Strawberry fallback'
Remove-Item function:global:Get-Command

$script:StrawberryPerlPath = $script:perlCandidate
function global:Get-Command { throw 'perl.exe is not on PATH' }
$noPathSelected = Get-NativePerl
Assert-True ($noPathSelected -eq (Resolve-Path -LiteralPath $script:perlCandidate).ProviderPath) 'Get-NativePerl must use the configured fallback when Perl is absent from PATH'
Remove-Item function:global:Get-Command

function global:Get-Command { [pscustomobject]@{ Source = 'C:\Git\usr\bin\perl.exe' } }
function global:Resolve-Path {
  param([string]$LiteralPath)
  [pscustomobject]@{ ProviderPath = $LiteralPath }
}
$gitSelected = Get-NativePerl
Assert-True ($gitSelected -eq $script:perlCandidate) 'Get-NativePerl must replace a Git/MSYS Perl candidate with the configured fallback'
Remove-Item function:global:Get-Command
Remove-Item function:global:Resolve-Path
$script:StrawberryPerlPath = $originalFallback
Remove-Item -LiteralPath $temporary -Recurse -Force

if ($failures.Count -gt 0) {
  $failures | ForEach-Object { [Console]::Error.WriteLine($_) }
  exit 1
}

Write-Output 'windows-desktop.Tests.ps1: passed'
