param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
try {
    if (-not $NoBuild) {
        & ./build.ps1
        & cargo build --locked --release --manifest-path gateway/Cargo.toml
        if ($LASTEXITCODE -ne 0) { throw 'The gateway build failed.' }
    }
    $gatewayExe = Join-Path $PSScriptRoot 'gateway/target/release/horizon-web-gateway.exe'
    $wasm = Join-Path $PSScriptRoot 'public/pkg/horizon_web_bg.wasm'
    if (-not (Test-Path -LiteralPath $gatewayExe) -or -not (Test-Path -LiteralPath $wasm)) {
        throw 'Build the browser and Windows gateway before packaging.'
    }
    $stage = Join-Path $PSScriptRoot ('target/package-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'public') -Destination (Join-Path $stage 'public') -Recurse
    Copy-Item -LiteralPath $gatewayExe -Destination $stage
    @'
$ErrorActionPreference = 'Stop'
$env:HORIZON_WEB_DIR = Join-Path $PSScriptRoot 'public'
& (Join-Path $PSScriptRoot 'horizon-web-gateway.exe')
if ($LASTEXITCODE -ne 0) { throw 'The Horizon gateway stopped with an error.' }
'@ | Set-Content -LiteralPath (Join-Path $stage 'run.ps1') -Encoding utf8
    @'
Horizon browser port — experimental Windows bundle

Extract the entire ZIP, open PowerShell in that folder, and run ./run.ps1.
Then open http://127.0.0.1:8787 in a WebGPU-capable browser.
No Rust installation or Microsoft application setup is needed for this bundle.
The default account flow uses the public Minecraft Launcher client ID.

Set a server address in the connection menu. This build speaks 26.3 / protocol
777. It connects and previews shaded server terrain; it is not the complete
playable Horizon game. Native textures, entities, inventory, movement, combat
and singleplayer are still pending. Online account approval and game admission
have not been verified end to end with an account.

The gateway normally listens only on localhost. Remote hosting needs HTTPS,
an explicit gateway key and allowed origins; see the source README.
Account API calls and game connections use the selected egress proxy. Microsoft's
verification webpage uses the browser's own network connection.

Source and setup: https://github.com/adoringlol/horizon/tree/web-port/web
'@ | Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding utf8
    $archive = Join-Path $PSScriptRoot 'target/horizon-web-windows.zip'
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $archive -Force
    Write-Host $archive
} finally { Pop-Location }
