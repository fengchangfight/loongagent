# ADR-0021 — A web fetch is an out-of-bounds read, and its text lands on disk

- **Status:** Accepted, implemented 2026-11-14
- **Up:** [../design/README.md](../design/README.md)
- **Date:** 2026-11-14
- **Decides:** How the agent reads a web page: which action class a fetch has,
  what reaches the model's context, and what happens to the rest of the page.
  Reads ADR-0007 §4 (autonomy is allocated per action class) and ADR-0020
  (YOLO never relaxes the irreversible) together and places a fetch between them.
- **Design docs:** [web-fetch.md](../design/web-fetch.md) ·
  [tools-permissions.md](../design/tools-permissions.md) ·
  [05-parity-and-synthesis.md](../05-parity-and-synthesis.md)
- **Code:** `crates/tools/src/web_fetch.rs` · `crates/tools/src/lib.rs` ·
  `crates/http/src/lib.rs` · `scripts/layering_rules.py`
- **Verify:** `cargo test -p loong-tools -p loong-http` · `./scripts/check.sh`

## Context

Every harness we measured reads web pages, and we did not. Claude Code ships
`WebFetch`, OpenCode ships `webfetch` (`docs/research/claude-code.md`,
`docs/research/opencode.md`), and `docs/05-parity-and-synthesis.md` has carried
the gap as "next: web_fetch … needs the egress decision" since the parity pass.
The gap showed up in use: handed a URL, the agent answered "this session makes no
network requests", which was true and unhelpful.

Two things blocked the tool, and neither was HTTP.

**First, the class.** ADR-0007 §4 puts network egress in `out_of_bounds`, and
`out_of_bounds` is `Ask`. ADR-0020 then says YOLO relaxes `out_of_bounds` but
never `irreversible`, and states the distinction in one line: "A `curl` that
fetched a page can be fetched again; a file that left the machine cannot be
recalled." A GET is therefore not irreversible — it is repeatable, and a
mistaken one costs a page nobody reads. But it is not `investigate` either: a
local read touches nothing outside the workspace, while a fetch tells a third
party that this machine exists, that this session is interested in that topic,
and it brings back text nobody vetted.

**Second, the context.** Both reference harnesses answer "what happens to the
page" differently and neither answer fits us. Claude Code converts HTML to
markdown and then feeds it to a small, fast model with the caller's question,
returning only that model's answer: the page never enters the main context, at
the cost of a second model call and a summary that can miss the detail asked
for. OpenCode converts to markdown and returns the page, capped: faithful and
simple, at the cost of tens of thousands of tokens per page and a direct path
for text on a web page to address the main model as if it were the person.

We have something neither design has: a file-based tool surface. `read_file`,
`grep` and `glob` already read the workspace, and the workspace is the cheapest
queryable store in the system.

## Decision

### 1. A fetch is `out_of_bounds`, never `irreversible`

`web_fetch` returns `ActionClass::OutOfBounds` for every argument, including a
malformed URL. Under the default ladder that is `Ask`: the person sees the URL
and answers. Under YOLO it runs, exactly as `curl` does today, because a page
can be fetched again. Nothing about it is a one-way door, so it must not claim
the class that exists for one-way doors.

The tool is in the registry and behind `Pdp::decide` like every other built-in
(`docs/extending/builtin-tool.md`): network egress may not bypass the ladder by
being "just a read".

### 2. The model gets an excerpt; the page goes to disk

A fetch writes the converted text under `.loongagent/fetch/` in the workspace and
returns to the model: the status, the content type, the character count, the
path of the saved file, the page title, and the first 6,000 characters. When the
page is shorter than that, the excerpt is the whole page and the result says so.

`.loongagent/fetch/` joins `projects/` and `control/` as state, not
configuration: it is git-ignored, it is inside the workspace so `read_file`,
`grep` and `glob` read it without an approval of its own, and deleting it costs
one refetch.

Rejected on the way here: keeping pages outside the workspace (a temp directory
is `out_of_bounds` for `read_file`, so every follow-up read would ask again), and
returning the whole page (the token cost is the smaller problem; the larger one
is §4).

### 3. The excerpt is data, not instructions

Every result carries one line saying the text below is untrusted content from the
web and is data, never instructions. The text between the markers is not
paraphrased and not summarised, so a page that says "ignore your previous
instructions" arrives saying exactly that — inside a labelled block, in a
result the model already knows is third-party.

This is why we do not summarise with a second model: the summariser is a second
chance to launder an instruction into the main context as if it were our own
prose, and it costs a network round trip we can avoid.

### 4. Redirects are reported, not followed

A `3xx` with a `Location` returns that absolute URL and does not re-fetch. The
model decides whether to ask again. Following silently would let a page move the
request to a host the person did not approve, which is the one way an approved
fetch becomes an unapproved one.

### 5. Refusals are for the cases where a page is not a page

The tool refuses, with a reason and no network call: a scheme other than
`http`/`https`; a host with no dot in it, or a loopback/private literal address
(`localhost`, `127.0.0.1`, `10.…`, `192.168.…`, `169.254.…`, `::1`) — that is a
request into the person's own network, not a read of the web, and `shell` plus
`curl` is the honest way to ask for it. It refuses after the response when the
content type is not text: a PDF or an image is not something this tool can turn
into text, and pretending otherwise spends the download to say so.

## Consequences

- The permission surface gains one `out_of_bounds` tool and no new class. The
  ladder, the YOLO rule and the hooks that match `builtin:out_of_bounds` all
  apply unchanged.
- A session without a person at the keyboard cannot fetch: `Ask` has no answer
  there, and the call fails with the URL in the message. That is the same
  behaviour `curl` already has, and it is the right one — an unattended session
  must not reach the network on its own.
- The workspace gains a state directory that grows until someone deletes it. No
  eviction policy is decided here: a page is small, the directory is ignored by
  git, and a cache with a policy nobody wrote is worse than a directory a person
  can `rm -rf`.
- Context cost per fetch is bounded and predictable: at most 6,000 characters,
  whatever the page weighs. The rest is a path away, reachable with `grep`.
- The HTML converter is ours: no dependency, and therefore no `libxml`-class
  surface. It handles what documentation and article pages are made of, and it
  will render a hostile `<table>` layout badly. That is acceptable for reading,
  and `crates/http` plus `shell` remain for the rest.

## Alternatives considered

1. **Return the whole page, OpenCode-style.** One less concept, and faithful.
   Rejected on two counts: the token cost per page is unbounded, and the page
   text enters the main context unlabelled by anything but its own markup.
2. **Summarise with a small model, Claude Code-style.** Best context economy,
   and the model we already have could be asked. Rejected: it puts a second
   model between the page and the person's question, spends a request per page,
   and gives an injection a paraphrase to hide in (§3).
3. **Cache pages under `$LOONGAGENT_HOME`.** Cleaner separation of state from
   workspace. Rejected: everything under the home is `out_of_bounds` for
   `read_file`, so the page would cost a prompt to read back — the tool would
   save tokens and spend approvals.
4. **A domain allowlist in configuration, fetches silent inside it.** The
   friendliest ergonomics, and what a mature product would add. Rejected for
   now: it silently answers the egress question ADR-0007 §4 answers with `Ask`,
   and an allowlist that defaults to empty is a config file nobody writes.
5. **Fetch through `shell` and `curl` and stop there.** No new code. Rejected:
   `curl` returns raw HTML to the context, which is the worst of both designs —
   unlabelled, unbounded, and unreadable.
