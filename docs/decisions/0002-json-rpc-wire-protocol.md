# ADR-0002 — JSON-RPC 2.0 as the wire protocol

- **Status:** Accepted
- **Date:** 2026-09-30
- **Decides:** AQ-2 (docs/03-architecture.md)

## Context

All surfaces (TUI, web, IDE) are thin clients of the local agent server
(ADR-0001). The interaction pattern is inherently **bidirectional**: clients
send intents (prompts, approvals, picks), the server pushes live events
(streaming tokens, tool calls, costs, steer/pick requests). IDE integration
goes through ACP (T-19), which is itself a JSON-RPC protocol.

## Decision

**JSON-RPC 2.0, bidirectional**, as the single client/server protocol.

- Transports: unix socket (named pipe on Windows) and stdio locally;
  WebSocket for remote/browser clients.
- Server→client traffic (event stream, steer/pick prompts, progress) travels
  as JSON-RPC notifications over the same connection.
- The API is **schema-first and versioned**, documented like an OpenAPI
  surface even though the transport is not HTTP.
- The ACP adapter is implemented as a **dialect of our own protocol** (ACP is
  JSON-RPC), not a second stack.

## Alternatives considered

- **REST + SSE (OpenCode style).** Rejected: ubiquitous tooling, but weak for
  bidirectional streaming and mid-run steer/pick; SSE is one-directional.
- **gRPC.** Rejected: strong typing/streaming, but painful for editor and
  browser clients and hostile to third-party client authors.

## Consequences

- One protocol to learn for all client authors; third-party clients are cheap
  to write (JSON-RPC libraries exist everywhere).
- ACP compatibility is nearly free — the IDE adapter speaks a related dialect.
- We own schema discipline: versioning policy and compatibility promises must
  be defined before external clients appear.
