# ADR-0001 — One binary, always client/server

- **Status:** Accepted
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-09-30
- **Decides:** AQ-1 (docs/03-architecture.md)

## Context

loongagent's ambition is "one agent core, multiple frontends": TUI, web, IDE
(via ACP), with sessions that roam across surfaces. The runtime topology of
the *local* agent server determines whether that is structural or bolted on.
Codex's lesson from the research: client/server is "cheap to do from day one,
painful to retrofit."

## Decision

**One binary, always client/server** (Codex app-server model).

- The single `loongagent` binary contains the local agent server.
- Invoking the CLI transparently spawns or connects to a per-user local server
  over a unix socket (named pipe on Windows).
- The TUI — and every other surface — is just a client of that server.
- Sessions live in the server: they survive client detach and can be
  re-attached from another surface (start in TUI, continue in web).
- Pi's single-process simplicity is preserved *inside* the server: session
  cores are boring in-process objects; the server shell around them is thin.

## Alternatives considered

- **In-process by default, server optional (Pi-style).** Rejected: multi-surface
  attach and session roaming become permanent special cases.
- **Always-on system daemon (OpenCode 2.0 direction).** Rejected for v1:
  heaviest install/ops footprint and worst first-run story; nothing prevents
  graduating the auto-spawned server into a managed daemon later.

## Consequences

- Every surface speaks the same protocol (AQ-2) from day one; no surface gets
  privileged in-process access.
- Server lifecycle management (spawn, reconnect, version-match between client
  and a running server) becomes a first-class concern of the binary.
- Session state ownership is unambiguous: the server owns it (AQ-3).
