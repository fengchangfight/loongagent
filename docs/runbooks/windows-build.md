# Windows build runbook

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0013](../decisions/0013-implementation-language.md)
- **Code:** `scripts/build-windows.ps1`, `scripts/build-windows.cmd`, `crates/cli/build.rs`, `crates/cli/assets/loongagent.rc`
- **Verify:** `powershell -ExecutionPolicy Bypass -File scripts/build-windows.ps1`

One command builds the agent for Windows, and the session you get is deliberately the
same session Ubuntu gets — same CLI, same slash commands, same keys. This file is the
doing: the prerequisites, the command, the parity table, and the differences that are
real rather than papered over.

**What has actually been run.** On Ubuntu 26.04 with `rustc 1.99.0` and mingw-w64
(gcc 13-posix), the workspace was cross-checked and cross-linked for
`x86_64-pc-windows-gnu`:

```bash
cargo check --workspace --target x86_64-pc-windows-gnu               # clean, no warnings
cargo check --workspace --all-targets --target x86_64-pc-windows-gnu # tests compile too
cargo build --release --locked -p loongagent --target x86_64-pc-windows-gnu
# -> 17 MB PE32+ executable, x86-64, console subsystem
```

The native MSVC path below is the supported one for a Windows machine and is the same
code (the platform switches do not distinguish `-msvc` from `-gnu`); it has simply not
been run by the person who wrote this file, who has no Windows host. Treat that
sentence as the current status, not as a disclaimer to skip.

Compiling and linking is all cross-compilation can prove. It cannot prove that the
Windows *interrupt* lands: that needs a real console, and it is the first thing to
check on a Windows host (press `ctrl-c` during a long turn — the turn stops, the
session survives). If it does not, the process group at spawn time and the handler
registration in `loong-server` are the two places to look.

## The one command

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1
```

From `cmd.exe`, or from Explorer, the same thing:

```bat
scripts\build-windows.cmd
```

The script checks the two things that actually break Windows builds (the Rust target
and a C compiler), runs `cargo build --release --locked --bin loongagent`, stages the
binary in `dist\windows\` next to a `BUILD.txt` naming the version, commit and
toolchain, and prints how to run it. Then:

```powershell
.\dist\windows\loongagent.exe tui --local
```

`--local` means "no control plane": no pack, no sync, no setup form. Drop it once a
control plane is configured. If the script is more than you want, the underlying
command is the same one Ubuntu uses:

```powershell
cargo build --release --locked -p loongagent     # -> target\release\loongagent.exe
```

The binary also carries the app icon from `crates/cli/assets/loongagent.ico`, compiled
from `crates/cli/assets/loongagent.rc`. Explorer and the taskbar show it instead of the
generic executable icon. A non-Windows target skips the resource entirely.

## Prerequisites

| Need | Why | How |
|---|---|---|
| Rust stable (rustup) | the workspace pins `stable` in `rust-toolchain.toml` | <https://rustup.rs> |
| A C compiler | not optional: `rusqlite` with the `bundled` feature compiles the SQLite amalgamation, and `ring` (TLS) compiles its own C and assembly | **MSVC:** Visual Studio Build Tools with the *Desktop development with C++* workload. **MinGW:** `x86_64-w64-mingw32-gcc` on PATH |
| `rust-std` for the target | only when the target is not the host ABI | `rustup target add x86_64-pc-windows-gnu` |

The icon resource needs a resource compiler: `rc.exe` from the Windows SDK for MSVC,
or `windres` from MinGW-w64 for GNU. Both ship with the C++ toolchain above; setting
`RC` to a specific compiler overrides the search.

Perl and NASM are **not** needed: the published `ring` crate ships pregenerated
assembly. The `minimal` rustup profile is enough.

If **Git for Windows** (or MSYS2, Cygwin, WSL) is installed and its `bin` directory is
on PATH, the agent will find `sh` and use it — see the shell row in the parity table.
That is worth doing: the tool classifier, the shell hooks and the built-in skills all
speak POSIX shell, and this is what makes them behave exactly as they do on Ubuntu.

## Three ways to get the binary

| Route | Command | Notes |
|---|---|---|
| Native, MSVC (recommended) | `scripts\build-windows.ps1` | default when `rustc` reports an `-msvc` host |
| Native, MinGW-w64 | `scripts\build-windows.ps1 -Target gnu` | also needs `rustup target add x86_64-pc-windows-gnu` |
| Cross from Linux | `cargo build --release --locked -p loongagent --target x86_64-pc-windows-gnu` | this is the route verified in the header; `apt-get install gcc-mingw-w64-x86-64` |

The three produce the same program; `--version` names the commit either way.

## The command surface is the same

Invocation:

| | Ubuntu | Windows |
|---|---|---|
| build | `cargo build --release` | `scripts\build-windows.cmd`, or the same `cargo` command |
| binary | `./target/release/loongagent` | `target\release\loongagent.exe` |
| full-screen | `loongagent tui` | `loongagent.exe tui` (PowerShell needs `.\` in front of a program in the current directory) |
| line mode | `loongagent repl` | `loongagent.exe repl` — no TTY needed, so pipes and scripts still work |
| headless | `loongagent run "…"` | `loongagent.exe run "…"` |
| usage | `loongagent --help` | `loongagent.exe --help` — the same tree |

In the session — these are identical on both platforms, and `/help` is generated by the
server from the skills and commands the session actually has:

| Command / key | Does |
|---|---|
| `/help` | what this session can do (skills, commands, MCP prompts) |
| `/new`, `/clear`, `/reset` | start a fresh session, keeping the old log |
| `/exit`, `/quit`, `/q` · `ctrl-c` (idle) · `ctrl-d` | leave |
| `/r` · `alt+r` | ask the last prompt again, of the model chosen now |
| `/1` … `/9` · `alt+1` … `alt+9` · `ctrl+n` / `ctrl+p` | choose a model; the bar shows the slots |
| `/expand` | last tool result in full |
| `/copy` | copy the transcript (OSC 52, plus a file path that always works) |
| `/mouse` | hand the mouse back to the terminal |
| `/yolo` | toggle approval prompts (the sandbox is untouched by this) |
| `/logo` | toggle the banner |
| `F2` | the context panel: what the next request will carry |
| `PgUp`/`PgDn`, wheel, `ctrl-PgUp`/`ctrl-PgDn` | scroll |
| `ctrl-c` while a turn runs | **stop the turn, keep the session** |

`ctrl-c` deserves the footnote it gets, because the mechanism is the one thing that
genuinely cannot be shared: on Ubuntu the client sends `SIGINT` to the server child,
and on Windows there is no `kill`, so the server is spawned with
`CREATE_NEW_PROCESS_GROUP` and the client sends a `CTRL_BREAK_EVENT` to that group
(`CTRL_C_EVENT` cannot be aimed at one group — it would hit the TUI too). The server
installs the matching handler: `SIGINT` on Unix, a console control routine on Windows.
The behaviour the person sees is the same: the running turn stops at the next step, the
server and the session survive it.

## What differs, and why

| Area | Ubuntu | Windows | Status |
|---|---|---|---|
| OS enforcement (PEP) | Landlock confines every process a tool spawns | **none implemented** | honest gap — see below |
| shell tool | `sh -c "…"` | `sh -c "…"` when `sh` is on PATH, otherwise `cmd.exe /C "…"` | works; command text must be `cmd`-compatible in the fallback |
| shell hooks | `sh hook.sh` | the same rule (`cmd.exe /C hook.cmd` in the fallback) | `.sh` hooks fail **closed** into `on_error` without a `sh` |
| killing a timed-out process tree | `kill(-pgid, SIGKILL)` | `taskkill /T /F /PID` | works; `taskkill` follows parent links instead of a group |
| config root | `~/.loongagent` (`$HOME`) | `%USERPROFILE%\.loongagent` (`HOME`, else `USERPROFILE`) | fixed — without `USERPROFILE` the whole config root silently fell back to `%TEMP%` |
| machine-wide config | `/etc/loongagent/profile.toml` | `%PROGRAMDATA%\loongagent\profile.toml` | fixed — the Unix path is drive-relative on Windows and could never be the MDM location |
| terminal encoding | UTF-8 throughout | the app sets the console code page to UTF-8 (`SetConsoleOutputCP`) | needed or the Chinese and box drawing mojibake |
| secret file (`env` holding keys) | written `0600` | inherits the user profile's ACL; no explicit ACL yet | acceptable, not yet proven |
| control plane | `/etc/loongagent-control`, `/var/lib/…`, bash init script | not laid out for Windows | run the control plane on Linux for now; the agent side is what this runbook covers |

### The gap that matters: no OS boundary on Windows

ADR-0007 says the kernel decides and the OS enforces. Landlock is Linux-only, and no
Windows primitive (AppContainer, Job Objects, ACLs) is wired up yet. Rather than let
the workspace pretend, the platform without a PEP is a *visible* one:

- `loongagent sandbox self-test` exits non-zero with `result: UNAVAILABLE … tool
  execution is not confined on this platform`;
- the server prints `[sandbox] WARNING: unconfined — …` before the first tool call;
- the kernel's confinement note reads `unconfined: …`.

So permissions still decide (`allow | ask | deny`, and the rule that fired is still in
the log), but on Windows the boundary is the policy, not the kernel. If that matters
for your use, run on Linux, or treat the next section as the work to pick up.

### Where the platform switches live

Six places, each with the other platform's answer beside it — a Windows port should
never be a scatter of `cfg` in unrelated code:

| Crate | File | Chooses |
|---|---|---|
| `loong-sandbox` | `src/landlock.rs` / `src/landlock_unsupported.rs` | Landlock on Linux; an honest `Unsupported` elsewhere |
| `loong-proc` | `src/lib.rs` (`Shell`, `prepare_process_group`, `kill_process_group`) | `sh` vs `cmd.exe`; `kill(-pgid)` vs `taskkill /T`; CRLF normalisation |
| `loong-tui` | `src/lib.rs` (`interrupt`) | `SIGINT` to a pid vs `CTRL_BREAK_EVENT` to a process group |
| `loong-server` | `src/lib.rs` (`handle_interrupt`, `install_interrupt_handler`) | signal handler vs console control routine |
| `loong-config` | `src/paths.rs` (`home_dir`, `system_config_dir`) | `HOME`/`USERPROFILE`; `/etc` vs `%PROGRAMDATA%` |
| `loong-tui-app` | `src/surface.rs` (`run`) | the console code page, once, before the alternate screen |

`loong-cli` sets the server child's creation flags and the demo's out-of-workspace
path; `loong-mcp` gives an MCP server its own process group so its tree can be torn
down too.

## Verifying a build

```powershell
.\dist\windows\loongagent.exe --version        # version, schema, commit
.\dist\windows\loongagent.exe config show      # every setting, and which layer set it
.\dist\windows\loongagent.exe sandbox self-test # expects UNAVAILABLE on Windows today
.\dist\windows\loongagent.exe tui --local      # then type /help
```

The executable should also show the dragon icon in Explorer, the taskbar and
Alt-Tab; a generic icon means the resource compiler was skipped.

Inside the session, the smoke test that exercises the real path is: ask for something
that writes a file (the policy decision, the rule that fired and the tool result land
in the stream), then press `ctrl-c` during a long turn and watch the turn stop without
the session ending. On Windows that last one is exercising the console control event
end to end.

For the cross-check from Linux (what CI would run first), see the header.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `linker 'link.exe' not found` | no MSVC C++ toolchain | install Visual Studio Build Tools with *Desktop development with C++*, or build with `-Target gnu` |
| `failed to run custom build command for libsqlite3-sys` / `ring` | same cause: a C compiler is missing | as above |
| `the Windows icon resource compiles` in `loongagent`'s build script | no `rc.exe` (MSVC) or `windres` (MinGW) | install the Windows SDK or MinGW-w64, or set `RC` to the compiler |
| `error[E0463]: can't find crate for 'std'` | the target's std is not installed | `rustup target add x86_64-pc-windows-gnu` |
| Chinese text or box drawing looks like garbage | the terminal is not UTF-8 | the app sets the code page itself; **Windows Terminal** is the recommended host, legacy conhost has weaker VT and Alt-key support |
| `alt+1` does nothing | the terminal eats Alt | use `/1` … `/9`, or `ctrl+n`/`ctrl+p` — the same fallbacks Ubuntu documents |
| every `shell` call fails to spawn | no `sh` and the command is not `cmd`-compatible | install Git for Windows (adds `sh`), or phrase the command for `cmd.exe` |
| `.sh` hook never allows anything | no `sh`; the hook fails closed | install Git for Windows, or write the hook as `.cmd` |

## Not done yet

- A real Windows PEP (AppContainer, or a job object with a restricted token), so
  `sandbox self-test` can prove a boundary on Windows the way Landlock does on Linux.
  This is the first thing to build.
- An explicit ACL on `%USERPROFILE%\.loongagent\env`, the file that holds API keys.
- Windows layouts for the control plane, and a PowerShell counterpart to
  `scripts/control-plane-init.sh`.
- A Windows job in `.github/workflows/ci.yml`. It is not there today because the gate
  (`scripts/check.sh`) is bash; the build half is one command — see the header — and
  could run on `windows-latest` as soon as the test half has a Windows story. Tests
  that need a POSIX shell are already skipped where there is none.
- Bracketed paste, on both platforms (it is not on either today, so the behaviour is at
  least consistent).

The other `scripts/*.sh` (check, demo, release, install-local) are bash: use WSL or Git
Bash for those on Windows. `scripts/build-windows.ps1` is their counterpart for the
build.
