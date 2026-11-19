# `make run-dev` for Windows without make (PowerShell): starts the dev databases
# (Docker Desktop) and runs the app with the dev profiles only (target\dev-config).
# Testing only: no app is built or installed, and passwords are not kept (see
# CONTRIBUTING.md, "Windows").
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

function Test-Docker {
    docker info 2>&1 | Out-Null
    return $LASTEXITCODE -eq 0
}

if (-not (Test-Docker)) {
    $desktop = Join-Path $env:ProgramFiles 'Docker\Docker\Docker Desktop.exe'
    if (-not (Test-Path $desktop)) { throw 'Docker is not running and Docker Desktop was not found' }
    Write-Host 'starting Docker Desktop...'
    Start-Process $desktop
    $ready = $false
    foreach ($i in 1..60) {
        Start-Sleep -Seconds 2
        if (Test-Docker) { $ready = $true; break }
    }
    if (-not $ready) { throw 'Docker did not become ready in 2 minutes' }
}

docker compose up -d --wait
if ($LASTEXITCODE -ne 0) { throw 'docker compose up failed' }
if (-not ((docker compose port ssh 22) -match ':52222$')) {
    throw 'The dev ports are taken by another compose project (docker compose ls); stop it first.'
}

$config = Join-Path 'target' 'dev-config'
New-Item -ItemType Directory -Force $config | Out-Null
$profiles = Join-Path $config 'profiles.toml'
if (-not (Test-Path $profiles)) { Copy-Item (Join-Path 'dev' 'profiles.toml') $profiles }

$env:TANGUL_CONFIG_DIR = $config
cargo run
