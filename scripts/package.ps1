<#
.SYNOPSIS
  One-command package for end users, on Windows.

.DESCRIPTION
      powershell -ExecutionPolicy Bypass -File scripts\package.ps1

  Produces, in release\ (git-ignored):
    loongagent-<version>\
      loongagent.exe               the release binary for this machine
      loongagent-vscode-<v>.vsix   the VS Code extension
      manual.html                  single-file quick-start manual (images inlined)

  The package is for people who will never see this repository: a binary, the
  extension, and a manual. Linux/macOS builds use scripts/package.sh instead;
  this script compiles only for the machine it runs on.

.PARAMETER Target
  Toolchain to build with: `auto` (default, the host's own ABI), `msvc` (Visual
  Studio Build Tools) or `gnu` (MinGW-w64).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\package.ps1
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [ValidateSet('auto', 'msvc', 'gnu')]
    [string]$Target = 'auto'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $version = ([regex]::Match((Get-Content Cargo.toml -Raw), '(?m)^version = "(.*)"')).Groups[1].Value
    $out = "release\loongagent-$version"
    $vscodeDir = 'clients\vscode'

    Write-Host "== [1/4] building loongagent $version (release, locked)"
    $cargoArgs = @('build', '--release', '--locked', '-p', 'loongagent')
    if ($Target -ne 'auto') { $cargoArgs += @('--target', "x86_64-pc-windows-$Target") }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    Write-Host "== [2/4] packaging the VS Code extension"
    Push-Location $vscodeDir
    try {
        if (-not (Test-Path node_modules)) {
            Write-Host "   installing extension dependencies (npm ci)"
            & npm ci
            if ($LASTEXITCODE -ne 0) { throw "npm ci failed" }
        }
        & npm run package:vsix
        if ($LASTEXITCODE -ne 0) { throw "vsix packaging failed" }
    } finally {
        Pop-Location
    }

    Write-Host "== [3/4] staging $out"
    if (Test-Path $out) { Remove-Item -Recurse -Force $out }
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    $exe = if ($Target -eq 'auto') { 'target\release\loongagent.exe' } `
           else { "target\x86_64-pc-windows-$Target\release\loongagent.exe" }
    Copy-Item $exe "$out\loongagent.exe"
    Copy-Item 'clients\manual\index.html' "$out\manual.html"
    $vsix = Get-ChildItem "$vscodeDir\*.vsix" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $vsix) { throw "no .vsix found under $vscodeDir" }
    Copy-Item $vsix.FullName $out

    Write-Host "== [4/4] done"
    Write-Host ''
    Write-Host "package ready at $out\:"
    Get-ChildItem $out | Format-Table -AutoSize | Out-String | Write-Host
    Write-Host 'Hand the whole folder to a user: put the exe on PATH, open manual.html,'
    Write-Host 'and (optionally) install the .vsix in VS Code.'
} finally {
    Pop-Location
}
