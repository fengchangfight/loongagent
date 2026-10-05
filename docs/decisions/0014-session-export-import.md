# ADR-0014 — Session export/import: file-based continuity, not sync

- **Status:** Accepted
- **Date:** 2026-10-04
- **Decides:** session portability. Extends ADR-0003 (session storage), refines
  ADR-0012 §6 (no cross-device *sync*), and stays inside ADR-0007/0008
  (policy is local and never travels)

## Context

The owner wants **full session context export and import**, so a session picked
up on another machine continues where it left off. This is deliberately *not*
the same thing as the sync that ADR-0012 §6 ruled out:

- **Sync** would mean the control plane holds session state — a new trust
  surface, a merge problem, and a service that must exist for the product to
  work. Rejected.
- **Export/import** means the user moves a **file**, exactly as they would move
  a dotfile or a database dump. The backend still holds no session state, so
  ADR-0012's trust claim survives intact — and §6 already named manual
  export/import as the continuity path. This ADR turns that path into a
  first-class feature.

ADR-0003 makes it cheap: the session **is** a self-contained JSONL event log,
the SQLite index is derived and rebuildable, and the tree lives in the same file
as `id`/`parentId`. But "continue on another machine" needs **reconciliation**,
not just a byte copy — and it has one sharp edge: an exported file must never be
able to carry authority with it.

## Decision

### 1. Export is a first-class command
`loongagent session export <id>` produces **one self-describing bundle**:

- the **session's event tree**, always **complete** — the `id`/`parentId`
  history *inside* the session (paths, retry branches) travels in full, because
  that history *is* the session. Trimming it is a different feature (pruning),
  not an export option;
- the **ancestor sessions**, which are **structurally required**, not merely
  explanatory: a child session references its parent's prefix rather than
  copying it (ADR-0015), so without the chain it cannot assemble its context;
- a **manifest**: bundle schema version, session id(s), export time, source
  workspace path, **git revision + dirty state**, profile names used, and an
  inventory of the plugins/MCP servers/skills the session depended on;
- **optionally** the file-history snapshots.

**Descendant sessions are not included by default** (owner decision,
2026-10-05). `--with-branches` adds the **whole subtree** or nothing at all — a
half-included tree would break lineage, which is worse than excluding it.
Rationale, recorded so the default is not mistaken for an oversight:

- **Continuation does not need them.** The parent's context already holds each
  subagent's structured artifact and summary (ADR-0006/ADR-0011); that is what
  the trunk needs to keep working.
- **They are the archival/review material**, not the working unit — the full
  record stays on the machine that produced it, inspectable there.
- **They are expensive to carry.** In the reference session imported during
  design, the main session was ~1,400 events while its 14 subagent branches were
  ~2,700 — including them roughly tripled the bundle.

The bundle is checksummed. A size and contents summary — **how many sessions,
how many events, how large** — is shown **before** writing, because the bundle
contains conversation content.

### 2. Import is a first-class command, and it re-binds rather than assumes
`loongagent session import <bundle>` verifies the checksum, rebuilds the index,
registers the session, and **re-binds the workspace**: if the path differs the
user picks the target directory, and a git-revision mismatch is **reported, not
enforced** (the code may legitimately have moved on).

Missing dependencies — a plugin, MCP server, skill, or profile the session used
— are reported as **warnings attached to the session**, not as silent
degradation.

### 3. Authority never travels: no approvals, no policy, no credentials
This is the security core of the decision:

- **Historical approval events are records, not grants.** Every future action is
  re-evaluated by the local PDP (ADR-0007) under the local policy layers
  (ADR-0008). Otherwise a crafted bundle becomes a privilege-escalation vector —
  "the previous me already approved `rm -rf`".
- **Resolved configuration is not imported.** The local layers re-resolve, with
  a visible diff when the result differs materially (ADR-0008).
- **Credentials and keys are never in a bundle** — only references, and those
  resolve locally or not at all (ADR-0008).

### 4. An imported session is untrusted input
Its content — tool results, file contents, model output — may contain injected
instructions. On resume it enters the context ledger (ADR-0006) as history like
any other content, and the autonomy ladder (ADR-0007) governs every action.
**Import executes nothing**, restores nothing silently, and grants nothing.

### 5. Identity and collision
An imported session keeps its original id when that id is **absent locally** —
it is the same session, and tree lineage stays coherent. If the id **already
exists**, import creates a new local id and records `importedFrom` (original id
+ bundle hash) so lineage survives without ambiguity. **There is no merge,
ever** — the design has no sync, so there is no merge to get wrong.

### 6. What is not in the bundle
The code itself (git carries it), org policy packs (they come from the control
plane or the mirror, ADR-0008), credentials, and anything the user did not
include.

### 7. Continuity is personal, not collaborative
The primary use case is **your own machine-to-machine continuity**. Sharing a
bundle with a colleague is possible, but it is not the collaboration model —
that remains git-shared knowledge (ADR-0010). Redaction/elision options exist
for the sharing case; sanitization is opt-in, never silently applied.

### 8. Worktrees are not portable
Worktree state (ADR-0011) does not travel; the session re-binds to a fresh
checkout. Included file-history snapshots are for **inspection and undo**, and
restoring file state is a separate, explicit action — never a silent overwrite
of the target tree.

## Consequences

- **Cross-machine continuity without a sync service.** ADR-0012 stays true: the
  backend holds no session state, and the trust boundary is unchanged.
- **The property that matters is structural, not procedural:** a session file is
  never an authority. No approvals, no policy, no credentials — the PDP decides
  everything again, locally.
- **Reconciliation is real product work**, not plumbing: path mapping, revision
  mismatch, missing plugins, and profile differences all need legible UX, or the
  feature is a foot-gun.
- **One format serves three jobs:** personal continuity, support/debugging
  ("send me the session"), and archival.
- **Bundle size is user-visible** because file-history inclusion is optional and
  the contents summary precedes the write.
- **Default export is the working unit:** the session plus its ancestors,
  without descendants — so "export and continue elsewhere" carries what
  continuation needs and nothing whose absence would be noticed only later.
- **Vocabulary resolved (ADR-0015).** The two tree levels are now named and
  separated: a session **contains** an event tree (`id`/`parentId`, paths and a
  head), and sessions relate to each other by **session lineage** (fork,
  delegation). "Branch" means a child session, never a path. This ADR's
  session-level wording is therefore exact, and ancestor sessions are
  **structurally required** — a child session references its parent's prefix
  instead of copying it, so without the chain it cannot even assemble context.
- **Deferred:** the concrete container format (compression, single file),
  redaction UX, schema-version migration policy, and whether an org may forbid
  export (a narrow-only policy key, ADR-0008).
