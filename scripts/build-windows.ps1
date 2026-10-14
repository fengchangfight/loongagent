<#
.SYNOPSIS
  Build a usable loongagent.exe for Windows, in one command.

.DESCRIPTION
  The whole point is that this is the *only* command a Windows user needs:

      powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1

  or, from cmd.exe / a double-click:

      scripts\build-windows.cmd

  It checks the two things that actually break a Windows build — the Rust target
  and a C compiler for bundled SQLite and the TLS backend (`ring`) — builds the
  `loongagent` binary in release mode, stages it in `dist\windows\` with a BUILD
  file naming the version, commit and toolchain, and prints the one line to run
  the TUI.

  What it deliberately does NOT do: invent a Windows-only command surface. The
  binary it produces has exactly the same CLI and the same in-session slash
  commands as the Ubuntu build (`loongagent tui`, `/help`, `/new`, `/exit`, …).
  See docs/runbooks/windows-build.md for the parity table.

.PARAMETER Target
  Toolchain to build with: `auto` (default, the host's own ABI), `msvc` (Visual
  Studio Build Tools) or `gnu` (MinGW-w64). Cross-building an -msvc binary from
  Linux is not supported here; use `gnu` with the documented mingw setup instead.

.PARAMETER OutDir
  Where to stage the binary. Default `dist\windows` (git-ignored).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Target gnu
#>
#Requires -Version 5.1
[CmdletBinding()]
param(
    [ValidateSet('auto', 'msvc', 'gnu')]
    [string]$Target = 'auto',
    [string]$OutDir = 'dist\windows'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Fail([string]$message, [string[]]$hint) {
    Write-Host ''
    Write-Host "loongagent: $message" -ForegroundColor Red
    foreach ($line in $hint) { Write-Host "  $line" -ForegroundColor Yellow }
    Write-Host ''
    exit 1
}

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    Write-Host "== loongagent: Windows build" -ForegroundColor Cyan
    Write-Host "   repository: $root"

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail 'cargo is not on PATH.' @(
            'Install the Rust toolchain (rustup): https://rustup.rs',
            'Then open a NEW terminal, so PATH is picked up.'
        )
    }
    $rustcVersion = (& rustc --version) -join ' '
    Write-Host "   $rustcVersion"

    # --- which ABI -----------------------------------------------------------
    $hostLine = (& rustc -vV) | Select-String '^host: ' | Select-Object -First 1
    if (-not $hostLine) {
        Fail 'could not read the host triple from `rustc -vV`.' @(
            'A stock rustup toolchain prints one; a wrapped rustc may not.'
        )
    }
    $hostTriple = $hostLine.ToString().Replace('host: ', '').Trim()
    # The architecture comes from the host rather than being written as x86_64, so an
    # ARM64 machine builds its own binary instead of a wrong one.
    $hostArch = ($hostTriple -split '-')[0]
    if ($Target -eq 'auto') {
        if ($hostTriple -like '*-pc-windows-gnu') { $Target = 'gnu' } else { $Target = 'msvc' }
    }
    $triple = "$hostArch-pc-windows-$Target"
    Write-Host "   target:     $triple"

    # --- is the target's std installed? --------------------------------------
    # Asked of rustup when rustup is what installed Rust; a toolchain installed by
    # other means has its host ABI and nothing to look up.
    $installed = @()
    if (Get-Command rustup -ErrorAction SilentlyContinue) {
        $installed = @(& rustup target list --installed 2>$null)
    }
    $hostHasIt = ($hostTriple -eq $triple)
    if (-not $hostHasIt -and ($installed -notcontains $triple)) {
        Fail "the Rust standard library for $triple is not installed." @(
            "rustup target add $triple",
            'Then run this script again.'
        )
    }

    # --- a C compiler, which is not optional --------------------------------
    # rusqlite (`bundled`) compiles the SQLite amalgamation and `ring` compiles its
    # own C/asm. Both locate a compiler themselves: the `cc` crate finds MSVC through
    # vswhere, MinGW through the `x86_64-w64-mingw32-gcc` name. A missing one shows up
    # as a link/compile error deep in a dependency, so the check is worth its noise.
    if ($Target -eq 'msvc') {
        $hasCl = [bool](Get-Command cl.exe -ErrorAction SilentlyContinue)
        # vswhere is how the `cc` crate finds Visual Studio; it is installed next to
        # every VS 2017+ install, including Build Tools.
        $vswhere = $null
        $pf86 = ${env:ProgramFiles(x86)}
        if ($pf86) {
            $candidate = Join-Path $pf86 'Microsoft Visual Studio\Installer\vswhere.exe'
            if (Test-Path $candidate) { $vswhere = $candidate }
        }
        if (-not $hasCl -and -not $vswhere) {
            Write-Host '   warning: no Visual Studio C++ toolchain found.' -ForegroundColor Yellow
            Write-Host '            Install "Visual Studio Build Tools" with the' -ForegroundColor Yellow
            Write-Host '            "Desktop development with C++" workload, or build with' -ForegroundColor Yellow
            Write-Host '            -Target gnu and MinGW-w64. Continuing: the build will say.' -ForegroundColor Yellow
        }
    }
    else {
        $cc = if ($hostArch -eq 'x86_64') { 'x86_64-w64-mingw32-gcc' } else { "$hostArch-w64-mingw32-gcc" }
        if (-not (Get-Command $cc -ErrorAction SilentlyContinue)) {
            Fail "the MinGW-w64 C compiler ($cc) is not on PATH." @(
                'Install it (MSYS2: pacman -S mingw-w64-x86_64-gcc, or the winlibs build),',
                'put its bin directory on PATH, and open a new terminal.'
            )
        }
    }

    # --- build ---------------------------------------------------------------
    Write-Host '== cargo build --release --locked -p loongagent'
    $cargoArgs = @('build', '--release', '--locked', '--bin', 'loongagent')
    if (-not $hostHasIt) { $cargoArgs += @('--target', $triple) }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        Fail 'the build failed (the compiler output above says why).' @(
            'Missing C toolchain?  See the prerequisites in docs/runbooks/windows-build.md.',
            'Stale build state?    cargo clean, then run this script again.'
        )
    }

    $exe = if ($hostHasIt) { 'target\release\loongagent.exe' } else { "target\$triple\release\loongagent.exe" }
    if (-not (Test-Path $exe)) { Fail "the build reported success but $exe is not there." @() }

    # --- stage ---------------------------------------------------------------
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
    $staged = Join-Path $OutDir 'loongagent.exe'
    Copy-Item -Force $exe $staged

    $version = (& $staged --version) -join ' '
    $commit = 'unknown'
    if (Get-Command git -ErrorAction SilentlyContinue) {
        $commit = ((& git rev-parse --short HEAD) -join '').Trim()
    }
    $build = @(
        'name:      loongagent',
        "target:    $triple",
        "commit:    $commit",
        "built at:  $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))",
        "toolchain: $rustcVersion",
        '',
        'run:',
        "  $staged tui            # full-screen session in this directory",
        "  $staged --help         # the same command surface as the Linux build",
        "  $staged sandbox self-test   # proves (or refuses to claim) the OS boundary"
    ) -join [Environment]::NewLine
    Set-Content -Path (Join-Path $OutDir 'BUILD.txt') -Value $build -Encoding UTF8

    Write-Host ''
    Write-Host "== built $staged" -ForegroundColor Green
    Write-Host "   $version"
    Write-Host ''
    Write-Host '   run the TUI:'
    Write-Host "     .\$staged tui"
    Write-Host '   same commands as Ubuntu: /help, /new, /exit, /1../9, alt+1..9, ctrl-c'
    Write-Host ''
}
finally {
    Pop-Location
}
