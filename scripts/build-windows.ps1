$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Resolve-Cargo {
    $cmd = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $homeDir = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
    $exe = Join-Path $homeDir "bin\cargo.exe"
    if (Test-Path $exe) { return $exe }
    throw "cargo not found. Run: . .\scripts\use-rust.ps1  (or install rustup from https://rustup.rs)"
}

$cargo = Resolve-Cargo
$cargoBin = Split-Path -Parent $cargo
if ($env:Path -notlike "*$cargoBin*") {
    $env:Path = "$cargoBin;$env:Path"
}

& $cargo test --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

& $cargo build -p witchat-app --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$meta = & $cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
$exe = Join-Path $meta.target_directory "release\witchat.exe"
$out = Join-Path $root "dist\windows"

& $cargo run -q -p witchat-xtask -- dist $exe $out
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Get-ChildItem $out | Select-Object Name, Length, LastWriteTime
Write-Host "Portable binary: dist\windows\witchat.exe (verify: Get-FileHash dist\windows\witchat.exe; compare with witchat.exe.sha256)"
