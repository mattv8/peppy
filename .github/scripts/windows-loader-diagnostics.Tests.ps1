$ErrorActionPreference = 'Stop'
$failures = [System.Collections.Generic.List[string]]::new()
function Assert-Equal {
  param($Expected, $Actual, [string]$Message)
  if ($Expected -ne $Actual) { $script:failures.Add("$Message (expected '$Expected', got '$Actual')") }
}

. (Join-Path $PSScriptRoot 'windows-loader-diagnostics.ps1')
$imports = @(ConvertFrom-DumpbinImports -Lines @(
  '    KERNEL32.dll', '             140019000 Import Address Table', '                     0 time date stamp',
  '                         336 GetLastError', '    example.dll', '                         04A ordinal 27',
  '                         ordinal ???', '  Summary', '        1000 .data'
))
Assert-Equal 3 $imports.Count 'Import parser must return only thunk rows'
Assert-Equal 'GetLastError' $imports[0].Symbol 'Import parser must retain named symbols'
Assert-Equal 'Ordinal' $imports[1].Kind 'Import parser must classify ordinals'
Assert-Equal 27 $imports[1].Symbol 'Import parser must retain ordinal values'
Assert-Equal 'UnknownOrdinal' $imports[2].Kind 'Import parser must not invent an ordinal'

$exports = @(ConvertFrom-DumpbinExports -Lines @(
  '  Section contains the following exports for example.dll', '    number of functions 9',
  '  ordinal hint RVA      name', '        1    0 00011000 NormalExport',
  '        2    A 00012000 HexHintA', '        3    F 00013000 HexHintF',
  '       16   10 00014000 HexHint10', '        4    1 00015000 ForwardedExport (forwarded to OTHERDLL.RealExport)',
  '        5    2          ForwardedNoRva (forwarded to OTHERDLL.NoRvaTarget)',
  '        6    3 00016000 ParenthesizedExport (OTHERDLL.OtherExport)',
  '  Summary', '        1000 .text'
))
Assert-Equal 7 $exports.Count 'Export parser must return only export rows'
Assert-Equal 'NormalExport' $exports[0].Name 'Export parser must retain names'
Assert-Equal 1 $exports[0].Ordinal 'Export parser must retain ordinals'
Assert-Equal 'HexHintA' $exports[1].Name 'Export parser must accept hexadecimal hint A'
Assert-Equal 'HexHintF' $exports[2].Name 'Export parser must accept hexadecimal hint F'
Assert-Equal 16 $exports[3].Ordinal 'Export parser must keep decimal ordinals beyond hint 0x10'
Assert-Equal 'HexHint10' $exports[3].Name 'Export parser must accept hexadecimal hint 10'
Assert-Equal 'OTHERDLL.RealExport' $exports[4].Forwarder 'Export parser must retain explicit forwarders'
Assert-Equal 'ForwardedNoRva' $exports[5].Name 'Export parser must accept forwarders with blank RVA'
Assert-Equal 'OTHERDLL.NoRvaTarget' $exports[5].Forwarder 'Export parser must retain forwarders with blank RVA'
Assert-Equal 'OTHERDLL.OtherExport' $exports[6].Forwarder 'Export parser must retain parenthesized forwarders'

$matchedExport = Find-DumpbinExport -Exports $exports -Import ([pscustomobject]@{ Kind = 'Name'; Symbol = 'NormalExport' })
Assert-Equal 'NormalExport' $matchedExport.Name 'Export lookup must match the exact-case name'
$wrongCaseExport = Find-DumpbinExport -Exports $exports -Import ([pscustomobject]@{ Kind = 'Name'; Symbol = 'normalexport' })
Assert-Equal $null $wrongCaseExport 'Export lookup must not match a wrong-case name'
$ordinalExport = Find-DumpbinExport -Exports $exports -Import ([pscustomobject]@{ Kind = 'Ordinal'; Symbol = 16 })
Assert-Equal 'HexHint10' $ordinalExport.Name 'Export lookup must match ordinals'

# dumpbin output routinely contains blank lines; these must bind and parse without error.
$blankImports = @(ConvertFrom-DumpbinImports -Lines @('', '    KERNEL32.dll', '', '                         336 GetLastError', ''))
Assert-Equal 1 $blankImports.Count 'Import parser must accept blank string elements'
Assert-Equal 'GetLastError' $blankImports[0].Symbol 'Import parser must keep symbols around blank lines'

$blankExports = @(ConvertFrom-DumpbinExports -Lines @('', '  ordinal hint RVA      name', '', '        1    0 00011000 NormalExport', ''))
Assert-Equal 1 $blankExports.Count 'Export parser must accept blank string elements'
Assert-Equal 'NormalExport' $blankExports[0].Name 'Export parser must keep exports around blank lines'

# Empty output (zero imports or zero exports) must bind and yield zero rows, not a bind error.
$emptyImports = @(ConvertFrom-DumpbinImports -Lines @())
Assert-Equal 0 $emptyImports.Count 'Import parser must accept an empty array and return zero imports'
$emptyExports = @(ConvertFrom-DumpbinExports -Lines @())
Assert-Equal 0 $emptyExports.Count 'Export parser must accept an empty array and return zero exports'

$emptyLookup = $null
try {
  $emptyLookup = Find-DumpbinExport -Exports @() -Import ([pscustomobject]@{ Kind = 'Name'; Symbol = 'NormalExport' })
} catch {
  $failures.Add("Export lookup must accept an empty export list without a bind error ($($_.Exception.Message))")
}
Assert-Equal $null $emptyLookup 'Export lookup over zero exports must report a missing symbol (null)'

$emptyLookupOrdinal = $null
try {
  $emptyLookupOrdinal = Find-DumpbinExport -Exports @() -Import ([pscustomobject]@{ Kind = 'Ordinal'; Symbol = 16 })
} catch {
  $failures.Add("Ordinal lookup must accept an empty export list without a bind error ($($_.Exception.Message))")
}
Assert-Equal $null $emptyLookupOrdinal 'Ordinal lookup over zero exports must report a missing symbol (null)'

if ($failures.Count -gt 0) { $failures | ForEach-Object { [Console]::Error.WriteLine($_) }; exit 1 }
Write-Output 'windows-loader-diagnostics.Tests.ps1: passed'
