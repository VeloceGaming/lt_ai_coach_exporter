param(
    [string]$SdkDir = "..\mod-sdk"
)

$ErrorActionPreference = "Stop"

$modRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$sdkInput = if ([System.IO.Path]::IsPathRooted($SdkDir)) {
    $SdkDir
} else {
    Join-Path $modRoot $SdkDir
}
$sdk = (Resolve-Path -LiteralPath $sdkInput).Path
$sdkToolchainFile = Join-Path $sdk "rust-toolchain.toml"
$Toolchain = Select-String -LiteralPath $sdkToolchainFile -Pattern '^\s*channel\s*=\s*"([^"]+)"' |
    ForEach-Object { $_.Matches[0].Groups[1].Value } |
    Select-Object -First 1
if (-not $Toolchain) {
    throw "Unable to determine toolchain from $sdkToolchainFile"
}
$deps = Resolve-Path -LiteralPath (Join-Path $sdk "deps")
$native = Join-Path $sdk "native"
$modApi = Get-ChildItem -LiteralPath $deps -Filter "libmod_api-*.rlib" | Select-Object -First 1
$serdeJson = Get-ChildItem -LiteralPath $deps -Filter "libserde_json-*.rlib" |
    Sort-Object Length -Descending |
    Select-Object -First 1
$engineAsset = Get-ChildItem -LiteralPath $deps -Filter "libengine_asset-*.rlib" | Select-Object -First 1
$engineCore = Get-ChildItem -LiteralPath $deps -Filter "libengine_core-*.rlib" | Select-Object -First 1

if (-not $modApi) {
    throw "libmod_api .rlib not found in $deps"
}
if (-not $serdeJson) {
    throw "libserde_json .rlib not found in $deps"
}
if (-not $engineAsset) {
    throw "libengine_asset .rlib not found in $deps"
}
if (-not $engineCore) {
    throw "libengine_core .rlib not found in $deps"
}

$cargoHomeBin = Join-Path $env:USERPROFILE ".cargo\bin"
if (Test-Path -LiteralPath $cargoHomeBin) {
    $env:PATH = "$cargoHomeBin;$env:PATH"
}

$env:RUSTUP_TOOLCHAIN = $Toolchain
$flags = @(
    "-L",
    "dependency=$deps",
    "--extern",
    "mod_api=$($modApi.FullName)",
    "--extern",
    "serde_json=$($serdeJson.FullName)"
    "--extern",
    "engine_asset=$($engineAsset.FullName)"
    "--extern",
    "engine_core=$($engineCore.FullName)"
)
if (Test-Path -LiteralPath $native) {
    $flags += @("-L", "native=$native")
}
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]31

cargo rustc `
    --release `
    --manifest-path (Join-Path $modRoot "Cargo.toml") `
    --target-dir (Join-Path $modRoot "target") `
    --lib `
    -- `
    --crate-type cdylib

if ($LASTEXITCODE -ne 0) {
    throw "cargo rustc failed with exit code $LASTEXITCODE"
}

$built = Join-Path $modRoot "target\release\lt_ai_coach_exporter.dll"
if (-not (Test-Path -LiteralPath $built)) {
    throw "Cargo build finished, but expected DLL was not found: $built"
}
Copy-Item -LiteralPath $built -Destination (Join-Path $modRoot "lt_ai_coach_exporter.dll") -Force
Write-Host "Build successful: $(Join-Path $modRoot "lt_ai_coach_exporter.dll")"

@(
    "lt_ai_coach_exporter.dll.exp",
    "lt_ai_coach_exporter.dll.lib",
    "lt_ai_coach_exporter.pdb"
) | ForEach-Object {
    $sidecar = Join-Path $modRoot $_
    if (Test-Path -LiteralPath $sidecar) {
        Remove-Item -LiteralPath $sidecar -Force
    }
}
