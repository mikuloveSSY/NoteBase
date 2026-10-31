param([string]$OutputDirectory = 'dist-delivery')
$ErrorActionPreference = 'Stop'
$binary = Join-Path $PSScriptRoot '../src-tauri/target/release/notebase.exe'
if (!(Test-Path -LiteralPath $binary)) { throw '先执行 npm run desktop:build -- --no-bundle' }
$package = Get-Content (Join-Path $PSScriptRoot '../package.json') -Raw | ConvertFrom-Json
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$archive = Join-Path $OutputDirectory ('NoteBase-' + $package.version + '-member4-windows-x64.zip')
Compress-Archive -LiteralPath $binary,(Join-Path $PSScriptRoot '../docs/成员四操作说明.md') -DestinationPath $archive -Force
$digest = Get-FileHash -LiteralPath $archive -Algorithm SHA256
($digest.Hash.ToLowerInvariant() + '  ' + (Split-Path $archive -Leaf)) | Set-Content ($archive + '.sha256')
Write-Output $archive
