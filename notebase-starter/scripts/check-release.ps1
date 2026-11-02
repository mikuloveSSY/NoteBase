#Requires -Version 7.0
param([string]$ExpectedVersion = '1.0.0')
$ErrorActionPreference = 'Stop'
$project = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$package = Get-Content (Join-Path $project 'package.json') -Raw | ConvertFrom-Json
$lock = Get-Content (Join-Path $project 'package-lock.json') -Raw | ConvertFrom-Json -AsHashtable
$tauri = Get-Content (Join-Path $project 'src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json
foreach ($v in @($package.version, $lock.version, $lock.packages[''].version, $tauri.version)) {
    if ($v -ne $ExpectedVersion) { throw "npm/Tauri版本不一致: $v" }
}
foreach ($path in @('src-tauri/Cargo.toml', 'crates/notebase-core/Cargo.toml')) {
    $source = Get-Content (Join-Path $project $path) -Raw
    if ($source -notmatch ('(?m)^version = "' + [regex]::Escape($ExpectedVersion) + '"')) { throw "$path 版本不一致" }
}
foreach ($path in @('src-tauri/Cargo.lock', 'crates/notebase-core/Cargo.lock')) {
    $source = Get-Content (Join-Path $project $path) -Raw
    $entries = [regex]::Matches($source, 'name = "notebase(?:-core)?"\r?\nversion = "([^"]+)"')
    $count = if ($path.StartsWith('src-tauri')) { 2 } else { 1 }
    if ($entries.Count -ne $count) { throw "$path 包数量不符" }
    foreach ($entry in $entries) {
        if ($entry.Groups[1].Value -ne $ExpectedVersion) { throw "$path 包版本不一致" }
    }
}
$ui = Get-Content (Join-Path $project 'src/main.ts') -Raw
if (!$ui.Contains("import { version } from '../package.json'")) { throw 'UI未使用统一版本来源' }
$packager = Get-Content (Join-Path $project 'scripts/package-portable.ps1') -Raw
if (!$packager.Contains("releaseStatus = 'stable'")) { throw '打包状态非stable' }
$exe = Join-Path $project 'src-tauri/target/release/notebase.exe'
if (Test-Path -LiteralPath $exe) {
    $v = (Get-Item -LiteralPath $exe).VersionInfo.ProductVersion
    if ($v -ne $ExpectedVersion) { throw "exe产品版本不一致: $v" }
}
"PASS: npm/Cargo/Tauri/lock/UI/packager/exe version $ExpectedVersion"
