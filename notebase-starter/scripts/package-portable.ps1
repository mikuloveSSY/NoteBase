#Requires -Version 7.0
param([string]$OutputDirectory = (Join-Path $PSScriptRoot '../dist-delivery'))
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$binary = Join-Path $projectRoot 'src-tauri/target/release/notebase.exe'
if (!(Test-Path -LiteralPath $binary -PathType Leaf)) { throw '先执行 npm run desktop:build -- --no-bundle' }
$package = Get-Content (Join-Path $projectRoot 'package.json') -Raw | ConvertFrom-Json
$tauri = Get-Content (Join-Path $projectRoot 'src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json
$lock = Get-Content (Join-Path $projectRoot 'package-lock.json') -Raw | ConvertFrom-Json -AsHashtable
if ($package.version -ne $tauri.version -or $package.version -ne $lock.version) { throw 'package、lock、Tauri版本不一致' }
foreach ($manifest in @('src-tauri/Cargo.toml', 'crates/notebase-core/Cargo.toml')) {
    $source = Get-Content (Join-Path $projectRoot $manifest) -Raw
    $match = [regex]::Match($source, '(?m)^version\s*=\s*"([^"]+)"')
    if (!$match.Success -or $match.Groups[1].Value -ne $package.version) { throw "$manifest 软件版本不一致" }
}
$revision = (& git -C $projectRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE) { throw '无法读取构建来源提交' }
$dirty = [bool](& git -C $projectRoot status --porcelain)
$name = 'NoteBase-' + $package.version + '-' + $revision.Substring(0,8) + '-windows-x64'
$outputRoot = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force $outputRoot | Out-Null
$destination = Join-Path $outputRoot $name
$archive = Join-Path $outputRoot ($name + '.zip')
if ((Test-Path -LiteralPath $destination) -or (Test-Path -LiteralPath $archive)) { throw '同一交付目录或压缩包已存在，请选择新输出目录；不会覆盖旧交付' }
New-Item -ItemType Directory $destination | Out-Null
$executable = Join-Path $destination 'NoteBase.exe'
Copy-Item -LiteralPath $binary -Destination $executable
Copy-Item -LiteralPath (Join-Path $projectRoot 'docs/Windows使用说明.md') -Destination (Join-Path $destination '使用说明.md')
$digest = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
@{
    product = 'NoteBase'; version = $package.version; architecture = 'windows-x64';
    sourceRevision = $revision; workingTreeDirty = $dirty;
    packagedAtUtc = [DateTime]::UtcNow.ToString('o');
    executable = 'NoteBase.exe'; sha256 = $digest;
    runtime = 'Microsoft Edge WebView2 Runtime'; releaseStatus = 'development-candidate';
} | ConvertTo-Json | Set-Content (Join-Path $destination 'build-manifest.json') -Encoding UTF8
($digest + '  NoteBase.exe') | Set-Content (Join-Path $destination 'SHA256SUMS.txt') -Encoding UTF8
$ownedFiles = @(Get-ChildItem -LiteralPath $destination -File | ForEach-Object FullName)
Compress-Archive -LiteralPath $ownedFiles -DestinationPath $archive
$archiveDigest = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
($archiveDigest + '  ' + (Split-Path $archive -Leaf)) | Set-Content ($archive + '.sha256') -Encoding UTF8
[pscustomobject]@{ executable=$executable; zip=$archive; sha256=$digest; revision=$revision; dirty=$dirty }
