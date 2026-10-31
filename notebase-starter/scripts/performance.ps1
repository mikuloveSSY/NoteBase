param(
 [Parameter(Mandatory=$true)][ValidateSet('generate','build','rebuild','query','watch','concurrent','accept','steady')][string]$Mode,
 [Parameter(Mandatory=$true)][string]$OutputRoot,
 [ValidateSet(10000,100000,200000)][int]$Count=100000,
 [ValidateRange(1,10000)][int]$Repetitions=100
)
$ErrorActionPreference='Stop'
$projectRoot=Split-Path $PSScriptRoot -Parent
$binary=Join-Path $projectRoot 'crates/notebase-core/target/release/examples/perf.exe'
if(-not [IO.Path]::IsPathRooted($OutputRoot)){throw 'OutputRoot must be absolute'}
$OutputRoot=[IO.Path]::GetFullPath($OutputRoot)
New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$sourceFiles=Get-ChildItem (Join-Path $projectRoot 'crates/notebase-core/src'),(Join-Path $projectRoot 'crates/notebase-core/examples') -Recurse -File | Sort-Object FullName
$sourceHashes=@($sourceFiles | ForEach-Object {@{path=[IO.Path]::GetRelativePath($projectRoot,$_.FullName).Replace('\','/');sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash}})
$record=@{
 timestampUtc=[DateTime]::UtcNow.ToString('o');mode=$Mode;count=$Count;repetitions=$Repetitions
 commit=(& git -C $projectRoot rev-parse HEAD);dirty=@(& git -C $projectRoot status --porcelain)
 os=[Runtime.InteropServices.RuntimeInformation]::OSDescription
 cpu=$env:PROCESSOR_IDENTIFIER;logicalProcessors=$env:NUMBER_OF_PROCESSORS
 executableSha256=(Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
 sourceHashes=$sourceHashes
 cachePolicy='No OS cache flush; query JSON distinguishes first observed and warm repetitions'
}
try{$record['freePhysicalMemoryKiB']=(Get-CimInstance Win32_OperatingSystem -ErrorAction Stop).FreePhysicalMemory}catch{$record['freePhysicalMemoryKiB']=$null}
try{$record['physicalMemoryBytes']=(Get-CimInstance Win32_ComputerSystem -ErrorAction Stop).TotalPhysicalMemory}catch{$record['physicalMemoryBytes']=$null}
try{$record['cpuName']=(Get-CimInstance Win32_Processor -ErrorAction Stop | Select-Object -First 1).Name}catch{$record['cpuName']=$null}
$record | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputRoot "$Mode-environment.json") -Encoding utf8
& $binary $Mode $OutputRoot $Count $Repetitions 2>&1 | Tee-Object -FilePath (Join-Path $OutputRoot "$Mode.log")
if($LASTEXITCODE -ne 0){throw "Performance run failed: $LASTEXITCODE"}
