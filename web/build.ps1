param([switch]$Debug)
$ErrorActionPreference = 'Stop'
Push-Location $PSScriptRoot
try {
    rustup target add wasm32-unknown-unknown
    if ($LASTEXITCODE -ne 0) { throw 'Could not install the Rust WASM target.' }
    $bindgen = Get-Command wasm-bindgen -ErrorAction SilentlyContinue
    if (-not $bindgen -or ((& wasm-bindgen --version) -ne 'wasm-bindgen 0.2.129')) {
        throw 'Install the pinned tool first: cargo install wasm-bindgen-cli --version 0.2.129 --locked'
    }
    $buildArgs = @('build', '--locked', '--target', 'wasm32-unknown-unknown', '--lib')
    $profile = 'debug'
    if (-not $Debug) { $buildArgs += '--release'; $profile = 'release' }
    & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'The browser build failed.' }
    & wasm-bindgen "target/wasm32-unknown-unknown/$profile/horizon_web.wasm" --target web --out-dir public/pkg
    if ($LASTEXITCODE -ne 0) { throw 'Generating the browser bindings failed.' }
    Write-Host 'Browser build ready. Start it with cargo run --manifest-path gateway/Cargo.toml'
} finally { Pop-Location }
