# 00 — Vision

## Name

**loongagent** — after the Chinese 龙 (loong), a mythical creature composed of
the strongest body parts of many animals (antlers of a deer, scales of a fish,
claws of an eagle…). The name is the design philosophy: take the best part of
every tool, and the composition becomes something none of the parts could be.

## Mission

Build a coding agent that combines the best ideas from the best tools —
Claude Code, Codex (OpenAI), OpenCode, Cursor, Pi, DeepSeek-style open-model
harnesses — without being a "second" anything. Where two great ideas contradict
each other, we make a deliberate, documented **trade-off** instead of an
impossible merge.

## Non-goals

- Not a second Claude Code / Codex harness / OpenCode / Pi / Cursor.
- Not a thin wrapper around one model provider.
- Not locked to a single surface or a single user role.

## Ambition (as stated by the owner)

1. **Every surface, user's choice.** The TUI, the web page and the IDE (as a
   thin ACP client, the third build target) are the product surfaces (ADR-0009);
   the REPL and headless `run` are the same brain in two more modes. Users pick
   their flavor; sessions roam across a machine's surfaces. Desktop and CI are
   not product surfaces (ADR-0009).
2. **Every engineering role.** Not just "software developer": frontend, backend,
   DevOps/SRE, QA/test, data engineering, and more. The agent should have
   role-appropriate tools, context sources, and workflows.
3. **Best-of-breed composition.** Systematically harvest the best design ideas
   from the existing landscape (see `research/` and `05-parity-and-synthesis.md`),
   and resolve contradictions through explicit trade-offs recorded in
   `decisions/`.

## Guiding principles (to be refined)

- **Design docs first.** Big decisions are discussed and recorded in `docs/`
  before code is written.
- **Model-agnostic by default** — open models (DeepSeek et al.) are first-class
  citizens, not an afterthought.
- **The harness is the product.** Model quality commoditizes; orchestration,
  context engineering, UX, and extensibility are the durable moat.
- **Cost-conscious by design.** No user should ever burn frontier-model tokens
  on a trivial or off-topic request. Expensive intelligence is spent on
  demand and on evidence, never by default or by habit (see T-04).
- **UX is the product.** Every role gets a deliberate "this is THE tool for me"
  moment: the wizard's lightning CLI, the manager's three-click cost insight,
  the PM's visual spec canvas, the newbie's beautiful web UI. Same brain,
  different faces; progressive disclosure of power; beauty is functional
  (see T-06).

### Principles adopted from Pi (owner-endorsed 2026-09-30)

- **The core is just "model + tool loop."** The minimal harness. Everything
  else is a visible, optional layer with good defaults — pluggable, because
  nobody knows the right shape yet, and the harness must be cheap to change.
- **Context engineering is paramount: nothing hidden, everything
  inspectable.** No injecting things behind the user's back that aren't
  surfaced in the UI. Documented session format; every token in context is
  visible.
- **Be careful and *mean* with context.** Frontier models are RL-trained to
  be coding agents — they don't need 10k-token system prompts. Spend context
  on the user's work, not on explaining the agent to itself.
- **Branching history is a killer feature.** Fork, retry, rewind — history is a
  tree, not a linear log: an **event tree** inside each session, and **lineage**
  between sessions (ADR-0015; T-12).
- **Steer, don't just watch.** Mid-run steering and structured picks are
  first-class interactions (T-12).
- **Junior-proof defaults, senior-proof depth.** Safe guided defaults for
  newcomers; total customization for experts (T-21).
- **One agent core, multiple frontends.**

## Open questions (seed list — grows during brainstorming)

- [x] ~~Project name?~~ → **loongagent** (decided 2026-09-26)
- [x] ~~Implementation language(s) for core vs. surfaces?~~ → **Rust** for the
  kernel, the servers and the TUI; **TypeScript** for the thin web client
  (ADR-0013)
- [x] ~~One shared core + thin surfaces, or surface-native apps over an API?~~
  → **one binary, always client/server** (ADR-0001); every surface is a
  **thin client** that renders the event stream and holds zero agent logic
  (ADR-0009 §2)
- [x] ~~What does "support all roles" mean concretely — tool packs, personas,
  permission profiles?~~ → **roles are data, not forked code**: a named profile
  plus a versioned context pack (ADR-0006 §7) and per-action-class autonomy
  offsets (ADR-0007 §4). The per-role *UX composition* itself is still T-06's
  open work.
- [x] ~~Local-first, cloud-first, or hybrid sessions?~~ → **local-first**: the
  session log stays local and the backend holds no session state (ADR-0012 §6),
  fronted by a deliberately small control plane — identity, packs, the model
  gateway and opt-in aggregate statistics (ADR-0012; T-05)
