# Dot-source so PATH persists in this PowerShell session:
#   . .\scripts\use-rust.ps1
$ErrorActionPreference = "Stop"
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$bin = Join-Path $cargoHome "bin"
$cargo = Join-Path $bin "cargo.exe"
if (-not (Test-Path $cargo)) {
    throw "cargo.exe not found at $cargo. Install rustup from https://rustup.rs then open a new terminal."
}
if ($env:Path -notlike "*$bin*") {
    $env:Path = "$bin;$env:Path"
}
Write-Host "cargo: $((Get-Command cargo).Source)"
& cargo --version
