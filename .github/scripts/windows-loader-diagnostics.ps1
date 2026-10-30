param(
  [Parameter(Position = 0)]
  [string]$ExecutablePath
)

$ErrorActionPreference = 'Stop'
$script:MaximumImages = 64

function ConvertFrom-DumpbinImports {
  param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Lines)

  $imports = [System.Collections.Generic.List[object]]::new()
  $currentDll = $null
  foreach ($line in $Lines) {
    if ($line -match '^\s{4,}([^\s:]+\.dll)\s*$') {
      $currentDll = $Matches[1]
      continue
    }
    if (-not $currentDll) { continue }
    if ($line -match '^\s{20,}[0-9A-Fa-f]+\s+([^\s]+)\s*$') {
      $imports.Add([pscustomobject]@{ Dll = $currentDll; Kind = 'Name'; Symbol = $Matches[1] })
      continue
    }
    if ($line -match '^\s{20,}(?:[0-9A-Fa-f]+\s+)?(?i:ordinal)\s+#?([0-9]+)\s*$') {
      $imports.Add([pscustomobject]@{ Dll = $currentDll; Kind = 'Ordinal'; Symbol = [int]$Matches[1] })
      continue
    }
    if ($line -match '^\s{20,}.*(?i:ordinal).*$') {
      $imports.Add([pscustomobject]@{ Dll = $currentDll; Kind = 'UnknownOrdinal'; Symbol = $line.Trim() })
    }
  }
  return $imports
}

function ConvertFrom-DumpbinExports {
  param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Lines)

  $exports = [System.Collections.Generic.List[object]]::new()
  foreach ($line in $Lines) {
    # Export rows: decimal ordinal, hexadecimal hint, and an 8-digit hexadecimal RVA.
    # Forwarded exports may have a blank RVA. Headers and section summaries do not
    # start with a decimal ordinal followed by a hex hint, so they are excluded.
    if ($line -notmatch '^\s*([0-9]+)\s+([0-9A-Fa-f]+)\s+(?:[0-9A-Fa-f]{8}\s+)?(\S.*?)\s*$') { continue }
    $ordinal = [int]$Matches[1]
    $description = $Matches[3].Trim()
    $name = ($description -split '\s+')[0]
    $forwarder = $null
    if ($description -match '(?i)(?:forwarder|forwarded to)\s+([^\s\)]+)') {
      $forwarder = $Matches[1].Trim('(', ')')
    } elseif ($description -match '^([^\s]+)\s+\(([^\)]+)\)$') {
      $name = $Matches[1]
      $forwarder = $Matches[2]
    }
    $exports.Add([pscustomobject]@{ Ordinal = $ordinal; Name = $name; Forwarder = $forwarder })
  }
  return $exports
}

function Find-DumpbinExport {
  param([Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Exports, [Parameter(Mandatory)][object]$Import)

  # Windows export names are matched case-sensitively, so use -ceq for names.
  if ($Import.Kind -eq 'Ordinal') {
    return $Exports | Where-Object { $_.Ordinal -eq $Import.Symbol } | Select-Object -First 1
  }
  return $Exports | Where-Object { $_.Name -ceq $Import.Symbol } | Select-Object -First 1
}

function Find-Dumpbin {
  $command = Get-Command dumpbin.exe -CommandType Application -ErrorAction SilentlyContinue
  if ($command) { return $command.Source }
  $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
  if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) { throw 'dumpbin.exe is not on PATH and vswhere.exe is unavailable' }
  $installation = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
  if ($LASTEXITCODE -ne 0 -or -not $installation) { throw 'Visual Studio C++ tools were not found' }
  $candidate = Get-ChildItem -LiteralPath (Join-Path $installation 'VC\Tools\MSVC') -Filter dumpbin.exe -File -Recurse |
    Where-Object { $_.FullName -match '\\bin\\Hostx64\\x64\\dumpbin\.exe$' } |
    Sort-Object FullName -Descending | Select-Object -First 1
  if (-not $candidate) { throw "dumpbin.exe was not found under $installation" }
  return $candidate.FullName
}

function Resolve-ImportModulePath {
  param([Parameter(Mandatory)][string]$Dll, [Parameter(Mandatory)][string]$ExecutableDirectory)

  if ($Dll -match '^(?i:api-ms-win-|ext-ms-win-)') {
    return [pscustomobject]@{ Path = $null; State = 'ApiSetVirtual' }
  }
  $directories = [System.Collections.Generic.List[string]]::new()
  $directories.Add($ExecutableDirectory)
  $directories.Add((Join-Path $env:WINDIR 'System32'))
  foreach ($directory in ($env:PATH -split ';' | Where-Object { $_ })) {
    $directories.Add($directory)
  }
  foreach ($directory in $directories | Select-Object -Unique) {
    $candidate = Join-Path $directory $Dll
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
      return [pscustomobject]@{ Path = (Resolve-Path -LiteralPath $candidate).ProviderPath; State = 'Found' }
    }
  }
  return [pscustomobject]@{ Path = $null; State = 'NotFound' }
}

function Invoke-Dumpbin {
  param([Parameter(Mandatory)][string]$Dumpbin, [Parameter(Mandatory)][string]$Option, [Parameter(Mandatory)][string]$Image)
  $output = @(& $Dumpbin /nologo $Option $Image 2>&1 | ForEach-Object { $_.ToString() })
  if ($LASTEXITCODE -ne 0) { throw "dumpbin $Option failed for $Image" }
  return $output
}

function Invoke-WindowsLoaderDiagnostics {
  param([Parameter(Mandatory)][string]$Path)

  if ($env:OS -ne 'Windows_NT') { throw 'Windows loader diagnostics must run on Windows' }
  $executable = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).ProviderPath
  $dumpbin = Find-Dumpbin
  $executableDirectory = Split-Path -Parent $executable
  Write-Output "Windows static loader diagnostics: $executable"
  Write-Output "dumpbin: $dumpbin"
  Write-Output 'This inspects PE tables only; it does not load or execute any image.'

  $queue = [System.Collections.Generic.Queue[string]]::new()
  $queue.Enqueue($executable)
  $visited = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
  $missingDlls = [System.Collections.Generic.List[string]]::new()
  $missingExports = [System.Collections.Generic.List[string]]::new()
  $unverified = [System.Collections.Generic.List[string]]::new()

  while ($queue.Count -gt 0 -and $visited.Count -lt $script:MaximumImages) {
    $image = $queue.Dequeue()
    if (-not $visited.Add($image)) { continue }
    $imports = @(ConvertFrom-DumpbinImports -Lines (Invoke-Dumpbin -Dumpbin $dumpbin -Option '/imports' -Image $image))
    Write-Output "Image: $image ($($imports.Count) parsed imports)"
    foreach ($group in ($imports | Group-Object Dll)) {
      $resolved = Resolve-ImportModulePath -Dll $group.Name -ExecutableDirectory $executableDirectory
      if ($resolved.State -eq 'ApiSetVirtual') {
        $unverified.Add("UNRESOLVED API-SET MAPPING: $($group.Name) required by $image")
        continue
      }
      if ($resolved.State -eq 'NotFound') {
        $finding = "MISSING DLL: $($group.Name) (required by $image; not found in EXE directory, System32, or PATH)"
        $missingDlls.Add($finding)
        Write-Output $finding
        continue
      }
      $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($resolved.Path).FileVersion
      Write-Output ("  {0} => {1} (version {2})" -f $group.Name, $resolved.Path, $(if ($version) { $version } else { 'unknown' }))
      $exports = @(ConvertFrom-DumpbinExports -Lines (Invoke-Dumpbin -Dumpbin $dumpbin -Option '/exports' -Image $resolved.Path))
      foreach ($import in $group.Group) {
        if ($import.Kind -eq 'UnknownOrdinal') {
          $unverified.Add("UNVERIFIED IMPORT FORMAT: $($group.Name)!$($import.Symbol) required by $image")
          continue
        }
        $export = Find-DumpbinExport -Exports $exports -Import $import
        $label = if ($import.Kind -eq 'Ordinal') { "#$($import.Symbol)" } else { $import.Symbol }
        if (-not $export) {
          $finding = "MISSING EXPORT: $($group.Name)!$label (required by $image; resolved to $($resolved.Path))"
          $missingExports.Add($finding)
          Write-Output $finding
        } elseif ($export.Forwarder) {
          $unverified.Add("PRESENT FORWARDER (unverified): $($group.Name)!$label => $($export.Forwarder)")
        }
      }
      if (-not $visited.Contains($resolved.Path)) { $queue.Enqueue($resolved.Path) }
    }
  }
  if ($queue.Count -gt 0) { $unverified.Add("IMAGE LIMIT REACHED: stopped after $($script:MaximumImages) images") }
  Write-Output 'Loader diagnostic summary:'
  Write-Output "  Images inspected: $($visited.Count)"
  Write-Output "  Missing DLLs: $($missingDlls.Count)"
  Write-Output "  Missing exports: $($missingExports.Count)"
  Write-Output "  Unverified mappings/forwarders: $($unverified.Count)"
  $missingDlls | ForEach-Object { Write-Output "  $_" }
  $missingExports | ForEach-Object { Write-Output "  $_" }
  $unverified | ForEach-Object { Write-Warning $_ }
}

if ($MyInvocation.InvocationName -ne '.') {
  if (-not $ExecutablePath) { throw 'Usage: windows-loader-diagnostics.ps1 -ExecutablePath <path-to-exe>' }
  Invoke-WindowsLoaderDiagnostics -Path $ExecutablePath
}
