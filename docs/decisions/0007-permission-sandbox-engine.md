# ADR-0007 — Permission & sandbox engine: the decision is in the kernel, the enforcement is at the OS

- **Status:** Accepted
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-10-04
- **Decides:** AQ-8 permission & sandbox question (docs/03-architecture.md),
  including the autonomy ladder (T-11) and the hook veto/trust gate (T-09)

## Context

Rules written in markdown are advisory by nature — they shape what the model
*tries*, not what the machine *permits*. The Replit/Lemkin incident is the
canonical proof: an agent was told not to touch production and did it anyway,
because the instruction lived in the prompt. Safety has to be structural.

Three existing decisions bound the design. ADR-0004 puts tools behind one
uniform interface — "a single chokepoint for permissions, hooks, cost
metering" — and keeps the kernel fixed because behavior that can be swapped can
hide. ADR-0003 makes every policy decision an event. ADR-0005 established that
org policy reaches the local install as policy packs, and already flagged that
hard budget caps are probably the one case where a policy must *forbid* rather
than advise. Ecosystem evidence adds two more constraints: repo-shipped hooks
were a CVE vector for Claude Code (untrusted repos must never auto-execute),
and Codex's Guardian-style LLM approval reviewer exists but is a semantic layer
on top of a deterministic one.

## Decision

**Policy decisions happen inside the kernel, at the single tool chokepoint.
Enforcement happens outside it, in the OS sandbox and the tool runner. The two
are deliberately separate components.**

Terminology is the standard XACML / NIST zero-trust set: **PDP** = policy
decision point, **PEP** = policy enforcement point, **PIP** = policy
information point, **PAP** = policy administration point.

### 1. PDP: a fixed part of the kernel, never a plugin
The PDP is middleware on the tool boundary, covering built-in tools exactly as
it covers plugin and MCP tools (the ADR-0004 chokepoint). It returns one of
**allow / ask / deny**, and every verdict carries **the rule that fired**
(decision provenance, mirroring the context ledger's "why is this in my
context?"). A plugin may advise or veto through hooks; it may not *be* the
decision maker, and it may not be inserted between the model and the tool.

### 2. PEP: enforcement is structural, not deliberative
The PDP's verdict is not the safety mechanism; the PEPs are. They are:

- the **OS sandbox** — seatbelt (macOS), Landlock + bubblewrap (Linux),
  Job Objects / restricted tokens (Windows);
- the **tool runner** — file writes, subprocess spawns, path resolution;
- **network egress** control.

A PDP bug must not by itself grant filesystem or network access, and a PEP bug
must still leave a recorded decision showing what *should* have happened.

### 3. PIP: layered policy, org wins
Policy inputs, lowest to highest precedence: built-in defaults < **org managed
(server-distributed policy packs, ADR-0008)** < project (repo-committed) < user
< session overrides. The org layer outranks project and user, which is what
makes it tamper-resistant in practice. Org hard caps are enforced twice: the
gateway **refuses** model spend it will not pay for (ADR-0005 §2), and the PDP
`deny`s the local actions a cap forbids — a local brain cannot route around a
ceiling it does not hold the key for.

### 4. Autonomy is allocated per action class, not per tool
The ladder (T-11) is about what an action *does*, so wrapping a dangerous
operation inside a different tool does not change its class:

| Class | Examples | Default |
|---|---|---|
| Investigate / read | grep, read, repo map, web search | **allow**, no prompt |
| Write inside the workspace | edit, create, run tests | **allow** by default; profiles may gate |
| Out-of-bounds | network egress, paths outside the workspace, destructive or irreversible commands, credential access | **ask**, always explicit |

Defaults are junior-proof: a new engineer gets a safe ladder without
configuring anything. Role profiles (T-06) shift the ladder as **data**
(profile → per-class offsets), never as forked code; senior profiles may widen
allow ranges, and the widening is itself a recorded policy.

### 5. Sandbox default-on; approval policy is an orthogonal axis; YOLO is a visible opt-out
The sandbox being on and the approval policy being strict are different
questions, and either can change without the other. YOLO mode exists as an
explicit opt-out, and opting out is itself an event: the session is marked, the
marking is visible in the UI, and it travels with the session in audit.
`sandbox self-test` (Codex pattern) verifies the platform sandbox actually
works before the first tool call. **v1 scope is the file-write boundary and
network egress**; the micro-VM tier is deferred to async runners (AQ-10), where
the code is not the user's own machine.

### 6. Approvals are first-class events
An approval is not a modal dialog that disappears: the request, the decision,
the **scope granted** (once / this action class for this session / persistent
for this project), and the reason are all events in the JSONL stream
(ADR-0003), replayable in the session's event tree (ADR-0015). **Denials are structured and fed
back to the model with a reason** (the exit-code-2 spirit) — a silent failure
teaches the model nothing and produces doom loops. Approval *UX* belongs to the
surfaces (AQ-11); the decision and its record belong to the kernel.

### 7. Hooks advise and veto at the PDP; repo-shipped hooks never auto-execute
Hooks (T-09) fire on lifecycle events and can block, mutate, or enrich; their
reasons are fed back to the model like any denial. The trust gate is absolute:
**hooks shipped inside a repository are untrusted content** and run only after
explicit, per-repo trust opt-in (bound to content, so a changed hook re-asks).

**Hook kinds (narrowed 2026-10-05).** v1 supports three transports for the same
verdict contract (allow / ask / deny + reason):

- **shell** — a command; exit code + stderr per the exit-code-2 protocol;
- **HTTP** — an endpoint returning the verdict (the firm's existing policy or
  scanning services plug in as configuration, not as hand-written `curl`
  plumbing). Because event payloads leave the machine, this is the one place the
  local-first promise has an explicit exception: it is a network action class
  under the PDP, org-pinnable, and off by default;
- **MCP tool** — reuses the T-15 server registry, scopes and org allow-lists.

**Deferred to v2:** *LLM* and *subagent* hooks. Both spend tokens and the second
recurses, so they belong with the Guardian reviewer (§8) and the goal evaluator
(T-23), not in v1.

**Event set.** The lifecycle events must at least cover: session start/end,
**user-prompt submit**, pre/post tool use, **permission requested/denied**,
subagent start/stop, **pre/post compaction**, file changed, and working-directory
changed. (Baseline adopted from Claude Code's hook surface; names are ours.)

### 8. Guardian-style LLM pre-review is v2, not v1
Deterministic rules plus approval modes are the floor. A semantic reviewer is
genuinely valuable — it catches "this *intent* is dangerous" cases that rules
can't name — but it is a new cost, a new latency source, and a new failure
mode, and it must sit *on top of* a working deterministic layer rather than
replace it. It plugs in behind the same decision interface when it arrives.

### 9. Everything is inspectable, including refusals
Every decision (allow / ask / deny), the rule that fired, every hook vote, and
every enforcement result is an event. "Why did it let me do that?" and "why
didn't it?" are both answerable from the log.

## Consequences

- **Two failure domains instead of one.** A decision bug cannot alone open the
  filesystem; an enforcement bug still leaves an auditable record of intent.
- **Role autonomy becomes data.** T-06's "autonomy varies by role" is a profile
  table, not a code path — consistent with the microkernel (ADR-0004) and
  inspectable by construction (T-13).
- **Approval fatigue is treated as a real design risk.** Scope widening and
  deterministic auto-allow for safe actions are the mitigations; ask-rate is a
  metric worth watching, because a prompt that fires constantly gets clicked
  through, which is the Lemkin failure with extra steps.
- **Prompt-injection containment improves structurally.** Untrusted content
  (web results, repo files, MCP output) can influence the model but cannot
  widen the ladder; the ladder is policy, and policy is not model-reachable.
- **Junior-proof by default, senior-proof by opt-in.** YOLO and widened
  allow-ranges exist, are visible, and are auditable.
- **Deferred:** the concrete rule/policy language and pack schema (rides on
  AQ-9 config layering), hook trust UX, network allow-list granularity,
  micro-VM tier and its interface for async runners (AQ-10), and the Guardian
  reviewer's model/budget policy.
