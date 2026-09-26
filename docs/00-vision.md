# 00 — Vision

- **Status:** Draft
- **Last updated:** 2026-09-26

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

1. **Every surface, user's choice.** Terminal TUI, desktop app, web app, IDE
   plugin (VS Code first, others later) — same brain, multiple faces. Users pick
   their flavor; sessions ideally roam across surfaces.
2. **Every engineering role.** Not just "software developer": frontend, backend,
   DevOps/SRE, QA/test, data engineering, and more. The agent should have
   role-appropriate tools, context sources, and workflows.
3. **Best-of-breed composition.** Systematically harvest the best design ideas
   from the existing landscape (see `research/` and `01-landscape-synthesis.md`),
   and resolve contradictions through explicit trade-offs recorded in
   `decisions/`.

## Guiding principles (to be refined)

- **Design docs first.** Big decisions are discussed and recorded in `docs/`
  before code is written.
- **Model-agnostic by default** — open models (DeepSeek et al.) are first-class
  citizens, not an afterthought.
- **The harness is the product.** Model quality commoditizes; orchestration,
  context engineering, UX, and extensibility are the durable moat.

## Open questions (seed list — grows during brainstorming)

- [x] ~~Project name?~~ → **loongagent** (decided 2026-09-26)
- [ ] Implementation language(s) for core vs. surfaces?
- [ ] One shared core + thin surfaces, or surface-native apps over an API?
- [ ] What does "support all roles" mean concretely — tool packs, personas,
      permission profiles?
- [ ] Local-first, cloud-first, or hybrid sessions?
