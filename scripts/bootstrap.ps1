# Onboard a fresh clone by installing repo hooks and pinned developer tools.
$ErrorActionPreference = "Stop"
Set-Location "$PSScriptRoot\.."
cargo xtask setup --install-tools
