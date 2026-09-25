# TPT Git Simple - build pipeline (Windows).
#
# Free edition (default):  ./build.ps1
#   cargo build --release --target wasm32-unknown-unknown (no pro feature)
#   wasm-bindgen --target web  ->  web repo public/apps/git-simple/ when
#   TPT_WEB_REPO is set (glue named git-simple.js, wasm keeps its _bg.wasm
#   name - the glue resolves it via import.meta.url), otherwise dist\web.
# Pro desktop edition:     ./build.ps1 -Pro
#   trunk build --release --features pro (its own independent wasm build,
#   into dist\ - NOT the wasm-bindgen path above) + cargo build the native
#   webview exe. Deliberately does not touch public/apps/git-simple/: that
#   is the free in-browser demo's bundle, and it must never be built with
#   --features pro, or the site would give away every paid feature (save/
#   commit, get latest/pull, push, guided conflict resolution, safe undo)
#   for free. The two editions are separate cargo builds writing to separate
#   directories on purpose - do not try to "reuse" one build for both.
#
# Env:
#   TPT_WEB_REPO  path to the tptsolutions.co.nz web repo; the app is copied
#                 to <repo>\public\apps\git-simple\ for the hub runner.
#                 Defaults to the known local checkout below if unset.

[CmdletBinding()]
param(
    # Build the Pro desktop bundle (--features pro) instead of the free one.
    # Never writes to the free edition's $Dest - see header comment.
    [switch]$Pro,
    # Free-edition only: skip the cargo build and re-run wasm-bindgen against
    # an existing release build.
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
Set-Location -LiteralPath $PSScriptRoot

$Crate = "tpt-app-git-simple"
$Target = "wasm32-unknown-unknown"
$OutName = "git-simple"
$Slug = "git-simple"
$DefaultWebRepo = "D:\Programming\2 WIP\TPT Electrician\tpt-electrician-nz-2"

Write-Host "== TPT Git Simple build ==" -ForegroundColor Cyan

if ($Pro) {
    Write-Host " edition: pro (save/get latest/push + conflict guidance + safe undo)"
    Write-Host "-- trunk build --release --features pro (standalone bundle for the desktop shell)" -ForegroundColor Cyan
    trunk build --release --features pro
    if ($LASTEXITCODE -ne 0) { throw "trunk build failed" }

    Write-Host "-- cargo build --release -p tpt-git-simple-pro (webview exe)" -ForegroundColor Cyan
    cargo build --release -p tpt-git-simple-pro
    if ($LASTEXITCODE -ne 0) { throw "desktop build failed" }
    Write-Host "   exe: target\release\tpt-git-simple-pro.exe (serves dist\)" -ForegroundColor Cyan
    Write-Host "-- done" -ForegroundColor Green
    Write-Host "   (public/apps/git-simple/ was not touched - run ./build.ps1 with no flags to refresh the free demo)"
    exit 0
}

Write-Host " edition: free (view-only repo browser)"
if (-not $SkipBuild) {
    Write-Host "-- cargo build --release --target $Target" -ForegroundColor Cyan
    cargo build --release --target $Target -p $Crate
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

$Wasm = "target\$Target\release\tpt_app_git_simple.wasm"
if (-not (Test-Path $Wasm)) { throw "missing wasm output: $Wasm" }

$BindgenOut = if ($env:TPT_WEB_REPO) { Join-Path $env:TPT_WEB_REPO "public\apps\$Slug" } else { "dist\web" }
Write-Host "-- wasm-bindgen --target web -> $BindgenOut" -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $BindgenOut | Out-Null
wasm-bindgen --target web --out-dir $BindgenOut --out-name $OutName $Wasm
if ($LASTEXITCODE -ne 0) { throw "wasm-bindgen failed" }

Write-Host "-- done" -ForegroundColor Green
Write-Host "   glue: $BindgenOut\$OutName.js (+ $OutName"_bg.wasm")"
if (-not $env:TPT_WEB_REPO) {
    Write-Host "   (set TPT_WEB_REPO to the web repo to copy straight into public\apps\$Slug\)"
}
