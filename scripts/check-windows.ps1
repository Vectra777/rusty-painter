# Run where Windows policy permits executing local builds. Does not launch
# the GUI, install tools, change security policy, or touch Git.
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') {
    throw 'Run this script on Windows.'
}
Get-Command cargo -ErrorAction Stop | Out-Null
$projectRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $projectRoot
try {
    New-Item -ItemType Directory -Force 'target/windows-debug' | Out-Null
    Start-Transcript -Path 'target/windows-debug/checks.log' -Force | Out-Null
    try {
        & rustc -Vv
        if ($LASTEXITCODE -ne 0) { throw 'rustc version check failed.' }
        & cargo -V
        if ($LASTEXITCODE -ne 0) { throw 'cargo version check failed.' }
        $checks = @(
            @('fmt', '--check'),
            @('clippy', '--all-targets', '--locked', '--', '-D', 'warnings'),
            @('test', '--locked'),
            @('check', '--locked', '--benches', '--features', 'bench'),
            @('build', '--locked', '--release')
        )
        foreach ($cargoArgs in $checks) {
            Write-Host "cargo $($cargoArgs -join ' ')"
            & cargo @cargoArgs
            if ($LASTEXITCODE -ne 0) {
                throw "cargo $($cargoArgs -join ' ') failed (exit $LASTEXITCODE). See target/windows-debug/checks.log."
            }
        }
    } finally {
        Stop-Transcript | Out-Null
    }
} finally {
    Pop-Location
}
