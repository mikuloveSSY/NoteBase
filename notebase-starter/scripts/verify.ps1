$ErrorActionPreference = 'Stop'
function Check([string]$command, [string[]]$arguments) {
    & $command @arguments
    if ($LASTEXITCODE) { throw "$command failed with exit code $LASTEXITCODE" }
}
Check cargo @('test','--locked','--manifest-path','crates/notebase-core/Cargo.toml')
Check cargo @('clippy','--locked','--manifest-path','crates/notebase-core/Cargo.toml','--all-targets','--','-D','warnings')
Check cargo @('fmt','--manifest-path','crates/notebase-core/Cargo.toml','--','--check')
Check cargo @('fmt','--manifest-path','src-tauri/Cargo.toml','--','--check')
Check cargo @('clippy','--locked','--manifest-path','src-tauri/Cargo.toml','--all-targets','--','-D','warnings')
Check npm @('test')
Check npm @('run','build')
$env:PLAYWRIGHT_CHANNEL = 'msedge'
Check npm @('run','test:e2e','--','--workers=1')
Check npm @('run','desktop:build','--','--no-bundle')
Check npm @('run','test:desktop')
