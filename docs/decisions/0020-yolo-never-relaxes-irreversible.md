# ADR-0020 — YOLO never relaxes an irreversible action

- **Status:** Accepted, implemented 2026-11-09
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-11-09
- **Decides:** What YOLO is allowed to skip. Extends ADR-0007 §4 (autonomy is
  allocated per action class) and §5 (YOLO is a visible opt-out): the opt-out
  covers asking, never the two things asking exists for.
- **Design docs:** [tools-permissions.md](../design/tools-permissions.md) ·
  [flows/tool-approval.md](../design/flows/tool-approval.md) ·
  [reverse-ui.md](../design/reverse-ui.md)
- **Code:** `crates/schema/protocol.toml` · `crates/sandbox/src/lib.rs` ·
  `crates/tools/src/lib.rs` · `crates/kernel/src/turn/loop.rs` ·
  `crates/config/src/layer.rs`
- **Verify:** `cargo test -p loong-sandbox -p loong-tools -p loong-kernel` ·
  `./scripts/check.sh`

## Context

Until now YOLO meant one thing: `Verdict::Ask` becomes `allow`. Every ruling the
ladder would have stopped at — network egress, paths outside the workspace,
privilege escalation — runs without a prompt, and only a configured or hook
`deny` still refuses. That is the right trade for a person who is watching the
agent and does not want to click forty buttons.

It is the wrong trade for two kinds of action, reported from use: deleting
things, and sending things out. Both share the property the other out-of-bounds
actions lack — **the consequence cannot be taken back.** A `curl` that fetched a
page can be fetched again; a file that left the machine cannot be recalled, and
a deleted one is gone. Asking *after* the agent already did it is asking about a
fact.

Distinguishing them by tool is not enough: the same `shell` runs `ls`, `rm`, and
`curl --data @secrets`. The distinction has to sit where ADR-0007 §4 already put
autonomy — on the action class.

## Decision

### 1. A fourth action class: `irreversible`

`ActionClass` gains `Irreversible` (wire `irreversible`): the action class of
things that cannot be undone or un-leaked. Its ladder slot defaults to **ask**,
like `out_of_bounds`, and the ladder stays four slots wide.

### 2. YOLO skips asking for every class but this one

The kernel's relaxation arm skips `Irreversible`. Under YOLO a download, a
package install, a write outside the workspace and an escalation still run
without a prompt; a deletion or an egress still stops and asks. The interface
shows the state as before — ADR-0007 §5 requires it — and now says plainly what
YOLO does not cover.

### 3. The class is not configurable upwards

The built-in default is `ask`, and ADR-0008's monotonicity means a profile may
only narrow it (`deny`). There is no configuration that widens `irreversible`
to `allow`, so the class cannot be talked out of its job by a config file: the
only way past it is answering the prompt.

### 4. The classifier names the two families

`classify_command` reports `Irreversible` for:

- **deletion** — a pipeline segment whose command is `rm`, `rmdir`, `shred`,
  `unlink`, `srm`, or `git rm`, or a segment containing `-delete` (the `find`
  idiom), `dd of=`, or `truncate`;
- **egress** — a segment whose command is `scp`, `sftp`, `rsync`, `ssh`, `nc`,
  `ncat`, `telnet`, `ftp`, `sendmail`, `mail`, or `git push` / `npm publish` /
  `cargo publish` / `gh release`, or a `curl`/`wget` carrying a payload
  (`-d`, `--data*`, `-F`, `--form`, `-T`, `--upload-file`, `--post-data`,
  `--post-file`, `-X POST|PUT|PATCH`). A plain download stays `out_of_bounds`.

Verb matching is token-precise on purpose. The existing tables match substrings,
which is tolerable while a hit only costs autonomy; a hit here costs a prompt
that the person has to be able to trust, so `alarm` is not `rm`.

## Consequences

- YOLO is no longer "never ask". A person who deletes files in a loop sees
  prompts again, and that is the point of the change, not a defect in it.
- `rm` asks even without YOLO, including inside the workspace: a workspace
  deletion was previously `write_workspace` → `allow`, with no prompt at all.
- The classifier remains a heuristic, and now one with a visible cost. It
  catches `curl --data @f` and `scp f host:`; it does not catch `python -c`
  opening a socket, a `base64` payload stuffed into a URL path, or a build
  script that uploads. YOLO therefore does not promise "nothing leaves"; it
  promises "the obvious ways to leak or delete stop and ask". The PEPs
  (ADR-0007 §2) stay the boundary: landlock walls off the file system, and
  network egress remains unconfined — this ADR narrows the prompts, not the
  hole.
- False positives are accepted: `rsync` from a remote host to local asks too,
  because telling the two directions apart from a command line is guesswork and
  a wrong guess in the safe direction only costs a click.

## Alternatives considered

1. **Match the rule string (`yolo:` / `builtin:out_of_bounds`) in the kernel's
   relaxation arm.** No new class, but the kernel would compare prose to decide
   a policy question, and a rule whose text changes would silently change what
   YOLO skips.
2. **A per-tool deny list.** The `shell` tool is the whole problem; a per-tool
   list cannot see inside it.
3. **Rely on the sandbox alone for deletion.** The file-write PEP does not cover
   `rm` of workspace files, and no PEP sees egress at all.
