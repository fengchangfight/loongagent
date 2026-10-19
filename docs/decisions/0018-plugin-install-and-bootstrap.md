# ADR-0018 — Plugins: install content by name, seat what a plugin asks for, execute nothing

- **Status:** Accepted, implemented 2026-10-15
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-10-15
- **Decides:** the parity gap "Git-based plugin marketplaces" ([05-parity-and-synthesis.md](../05-parity-and-synthesis.md) §A6), which ADR-0004 called "content bundles" and ADR-0008 §6 left to signed packs; and what a Claude Code plugin's `SessionStart` hook becomes on this harness
- **Builds on:** ADR-0004 (microkernel: edges are pluggable, the kernel is fixed), ADR-0006 (progressive disclosure), ADR-0007 §7 (hooks are advisory and gated), ADR-0008 (layered configuration), T-18 (skills and commands as one concept)

## Context

The field's plugin format is a bundle of four things: **skills, hooks, subagents and MCP
servers**, distributed from git (Claude Code marketplaces, and every harness that has
since copied the layout). We had none of the distribution half. What we did have turned
out to be most of the compatibility half, and that was measured rather than assumed:
pointing `skill_dirs` at a checkout of [obra/superpowers](https://github.com/obra/superpowers)
loads **all fifteen of its skills** with the loader as it already stands, frontmatter and
all — because "markdown with a `description`, body on invoke" is the same idea in both
places. What was missing was a way to *get* a checkout, and a way to say so that survives
being said twice.

Two constraints decided the shape of the answer.

**Hooks and MCP servers execute, and that is exactly why they are gated.** A repo-shipped
hook is inert until a human runs `loongagent hooks trust` in that repository (ADR-0007 §7);
an MCP server is inert until `loongagent mcp trust`. Both gates exist so that *having a
repository* can never mean *running its code*. If `plugin add` installed hooks, it would be
a way around that gate, and it would turn "install a well-known skill library" into "let a
third party run a shell script in every repository you open".

**Our hooks are veto-only on purpose.** A shell hook answers allow/ask/deny; its stdout is
parsed for a verdict and nothing else. There is no field that puts hook text into the
prompt — not for `session_start`, not for any event. That is what makes it safe to run a
hook that came with a repository at all.

And that is precisely the problem, because the one thing Superpowers asks a harness to do
at session start is *put text into the prompt*: its `hooks/hooks.json` fires `SessionStart`,
runs `hooks/session-start`, and injects the full body of its `using-superpowers` skill as
`additionalContext`. On the harnesses that support it, this is what makes the system work —
the discipline is "check for a relevant skill before responding", and a discipline that
lives behind a description the model may skip is not a discipline.

So the gap was real, and the interesting question was not "how do we run their hook" but
"what is the native form of what their hook is for".

## Decision

### 1. A plugin is a checkout with a manifest under one of two roots

```text
~/.loongagent/plugins/<name>/            # user scope: every project
<workspace>/.loongagent/plugins/<name>/  # project scope: this repository
```

`plugin add` writes `.loongagent-plugin.toml` inside the checkout. **A directory without a
manifest is not a plugin**: `git clone` into the plugins root installs nothing, and every
installed plugin got there through a command a person typed. This is the same trust story
`.loongagent/skills` already has — content that enters context is put there by an explicit
act — with the difference that a user-scope install follows the person into repositories
they did not write, so the act has to be deliberate.

A project install shadows a user install of the same name, and the shadow is reported
(`plugin list`, and a `[plugin]` line at session start) rather than silently resolved.

The manifest names where the skills live, and that name has to land **inside the
checkout**. `Path::join` hands an absolute path back unchanged and `..` walks up, so a
manifest saying `"/etc"` or `"../elsewhere"` would point the loader at a directory the
plugin does not own — and the manifest is a file a person is invited to edit. No install
produces such a path (the three sources are `--skills`, the catalogue, and what the
checkout shows), so it is refused where the manifest is read rather than trusted and
acted on later.

### 2. One command, and the common case is one word

```bash
loongagent plugin add superpowers        # a built-in catalogue entry, no URL to remember
loongagent plugin add obra/superpowers   # owner/repo
loongagent plugin add https://github.com/obra/superpowers
loongagent plugin add ./my-skills        # a directory; copied, if it is not a git checkout
loongagent plugin add obra/superpowers@v6.4.2   # a branch or tag
loongagent plugin list | update <name> | remove <name>
```

The catalogue in `crates/plugin` is a *catalogue*, not a marketplace: one entry per plugin
that has been looked at, with no index to fetch and nothing fetched until asked. Adding an
entry is a code change, which is the honest cost of claiming a plugin is worth installing.

Fetching is `git clone --depth 1`, because that is the same operation every other harness
performs and it gives `update` something to do. It runs with `-c http.version=HTTP/1.1`:
plain `git clone` hung past 120 s on this network while the same clone over HTTP/1.1
finished in seconds, and an install that appears to hang is worse than one that fails.

### 3. Only skills are installed

Not hooks, not subagents, not MCP servers. A plugin that ships hooks is not hidden — the
checkout is a directory anyone can read, and `plugin add` says so out loud:

```text
note: this plugin ships hooks/, which loongagent does not install or run — a hook is a
      per-repository trust decision (`hooks trust`), never a side effect of installing content
```

Subagents and MCP servers are not rejected so much as absent: we have no subagent
definitions yet, and an MCP server already has a gate of its own. **They are also not
flagged yet** — the two probes above are `hooks/` and nothing else, so a plugin shipping
`agents/` or `.mcp.json` is left behind silently rather than with a note. Closing that is
a small addition to the same function; leaving it unsaid would be the worse half of it.

### 4. `bootstrap`: the native form of the start-of-session injection

A plugin's manifest may name skills whose **bodies** are seated in the system prompt of
every session:

```toml
name = "superpowers"
source = "https://github.com/obra/superpowers"
skills = "skills"
bootstrap = ["using-superpowers"]
```

The kernel resolves those names against the catalogue it just built and appends the bodies
to the pinned `SystemPrompt` entry, under a line that says why they are there. It is the
same information the hook would have injected, carried by the loader that already exists,
with no third-party script executing and nothing to trust per repository.

The manifest may also carry `notes`: a line or two of harness adaptation, for a plugin
written against another harness's tool names. Superpowers' note says that its
`superpowers:<name>` spelling means the skill `<name>` here and that there is no todo tool.

Validation follows from the same principle as the rollback in §5: `plugin add` refuses a
plugin whose declared bootstrap skill is not one of its own. Otherwise the install would
succeed and the *next session* would refuse to open — the worst place to learn that a
manifest only half makes sense.

### 5. An install that would break the catalogue does not stay

`plugin add` loads the catalogue before the install (so "was it already broken?" has an
answer), fetches, then loads it again with the new plugin in place. If the second load
fails — a skill name collides with one the workspace already defines, which the skills
loader refuses loudly — the new plugin is deleted and the error is returned with the
rollback stated.

Installs are staged in a temporary directory inside the plugins root and renamed into
place, so a clone that dies halfway leaves nothing for the next session to trip over. The
plugin's own skills are parsed before the manifest is written.

An **update** cannot be staged that way — it is the same checkout, moved forward — so it
earns its place differently. The fetched commit's skills are parsed *before* the manifest
is rewritten, and a commit that does not parse is put back on the one that did, with the
rollback stated in the error. The manifest is rewritten from the copy read before the
fetch rather than left as the checkout now has it: a repository may commit a
`.loongagent-plugin.toml` of its own, and `reset --hard` restores tracked files, so
anything less than an explicit rewrite would let a failed update seat whatever that file
says.

### 5b. The name alone, when two installs share it

A project install shadows a user one (§1), which makes the bare name ambiguous — and an
ambiguity resolved silently is how the shadowed copy becomes impossible to reach, since
`plugin list` reports it and no command could name it. `plugin update` and `plugin remove`
therefore take `--project` / `--user`; without either, a name installed in both scopes is
a refusal that prints both paths and asks, and a name installed in one scope still needs
no flag. The scope is also what makes the rollback in §5 safe: `plugin add` removes the
copy it just installed *in the scope it installed into*, never a same-named plugin in the
other one.

### 6. A root that carries the workspace shape is read in that shape

`scan_root` used to read every root in both spellings: `skills/<name>/SKILL.md` **and**
flat `<name>.md` at the root. That made pointing a skill directory at a plugin checkout
fail the whole load, because a repository's `README.md`, `AGENTS.md` and release notes sit
next to its `skills/` and none of them has frontmatter.

A root that has a `skills/` directory is now read in the workspace shape only: `skills/`
and `commands/`. The cost is named rather than hidden — a stray `<name>.md` beside a
`skills/` directory is no longer a skill, and `commands/<name>.md` is where it belongs.
A root that *is* a directory of skills (`skill_dirs = ["~/team-skills"]`) is unchanged.

## Alternatives considered

- **Signed packs (ADR-0016) as the distribution channel.** Rejected for public content.
  Packs are org policy: admin-authored, Ed25519-signed, pinned out of band, offline-verifiable.
  A community skill library is none of those things, and stretching the envelope to cover it
  would put "whatever the pack says" in the org layer. Signature and trust tiering for
  *third-party* content remains the WASM story of ADR-0004 — future, and separate.
- **Porting the `SessionStart` hook, i.e. giving hooks a context channel.** Rejected: it
  would make repo-shipped scripts a path into the prompt, which is the one thing the hook
  design deliberately refuses. `bootstrap` gives the same effect through a channel that is
  already audited (skills) and already inspectable (a file in a checkout you can read).
- **Vendoring plugin skills into `.loongagent/skills/`.** Rejected: no provenance, no
  update, and it duplicates content into repositories that then diverge.
- **Auto-activating any directory under the plugins root.** Rejected: it would make
  `git clone` an installation, which is exactly the side effect §1 exists to prevent.
- **A network marketplace index.** Rejected for now: a catalogue in the binary is small,
  reviewable, and cannot be pointed somewhere hostile by configuration.

## Consequences

- **The cost is named.** With Superpowers installed, every session pays ~1.1 k tokens of
  skill descriptions, plus ~0.75 k for the always-on bootstrap — measured, not estimated.
  `plugin list` and `skills list` print those numbers, and `disable_skills` is the escape
  hatch for a single skill.
- **Name collisions are a hard stop, at install time.** Two plugins offering `brainstorming`
  cannot both be installed, and the second install says which name collided and rolls itself
  back. Making the loader pick one would make `/brainstorming` depend on directory order.
- **Bootstrap bodies are in the pinned system prompt**, so compaction cannot evict them
  (ADR-0006). The skill names are already in the session log through `skills.loaded`; the
  prompt text itself is not logged, exactly as the catalogue descriptions are not.
- **Not implemented, deliberately:** a plugin index or search, per-plugin enable/disable
  (use `disable_skills`), signatures or trust tiers, subagent definitions, plugin hooks or
  MCP servers, and per-plugin version pinning beyond `--ref`.
- **Scope of the surface:** the CLI only. There is no `/plugin` in the TUI or the browser
  page; both would be a form over the same four functions, and neither is on the critical
  path of "let people install Superpowers".
