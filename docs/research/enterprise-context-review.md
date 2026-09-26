# Enterprise Context & Review Tooling — Augment, Cody, Greptile, CodeRabbit, Tabby

> How vendors index and retrieve at 10M+ LOC scale, whether embedding-RAG survives there, where PR-review bots add vs destroy value, what self-hosted buyers demand, and what loongagent's enterprise context strategy should be.

**Researched: 2026-09-26**

---

## 0. Category thesis (cross-cutting)

- The 2026 consensus on code retrieval is **"structural + lexical first, embeddings optional"**. Claude Code, Devin-style agents and Cursor's current agent lean on grep, file trees, AST/symbol graphs and explore-subagents; Cursor's current docs state it "does not upload file paths or code to build a search index, and it does not store embeddings of your codebase" for its [Instant Grep engine](https://cursor.com/docs/agent/tools/search). Meanwhile Cursor's own [semantic-search blog post](https://cursor.com/blog/semsearch) reports +12.5% answer accuracy (up to 23.5% model-dependent) and +2.6% code retention on 1,000+ file repos from keeping semantic search — so the honest answer is **hybrid wins, pure embedding RAG loses**. A July 2026 [forum thread](https://forum.cursor.com/t/what-do-you-think-about-cursor-removing-the-codebase-indexing-settings/165899) confirms Cursor removed the visible codebase-indexing settings page, hiding/de-emphasizing the embedding pipeline.
- The clear-eyed summary: ["Why Cursor, Claude Code, and Devin use grep, not vectors"](https://www.mindstudio.ai/blog/is-rag-dead-what-ai-agents-use-instead) — code is a dependency graph, not prose; RAG chunk-misses fail silently and compound into broken generated code; grep fails loudly; AST navigation respects structure; 128K–1M token windows cover medium repos outright.
- PR-review bots have converged on **the same value stack**: diff analysis → codebase context → noise suppression via learning → one-click handoff to a coding agent to apply fixes. The differentiators are (a) context depth (diff-only vs full-repo graph), (b) false-positive economics, (c) git-platform coverage, (d) deployment model.
- Self-hosted buyers demand: no code leaving the network, SSO/SAML/LDAP, audit logs, RBAC, air-gap support, BYO-LLM endpoints, and SOC 2-style process evidence even for on-prem software.

---

## 1. Augment Code

### What it is
- Maker: Augment Code (founded 2022; Igor Ostrovsky, ex-Pure Storage chief architect, and Guy Gur-Ari, ex-Google research scientist). Positions itself as "The Software Agent Company."
- Target users: enterprise engineering teams with very large multi-repo/monorepo estates where grounding, not model IQ, is the bottleneck.
- Surfaces: VS Code + JetBrains extensions, **Auggie CLI**, **Cosmos** ("software factory": custom triggers + specialized agents to automate SDLC loops, including automated PR review), Slack agent, **Context Engine MCP/SDK**, and an open-source (MIT) [`context-connectors`](https://github.com/augmentcode/context-connectors) library that indexes GitHub/GitLab/Bitbucket repos and websites into local or S3 stores and serves them via CLI, agent, or MCP (stdio + HTTP). ([Augment docs intro](https://docs.augmentcode.com/introduction))
- Pricing ([augmentcode.com/pricing](https://www.augmentcode.com/pricing), as of 2026-09): **Standard $20/mo flat** (up to 50 seats, $20 usage included), **Business $100/mo flat**, Enterprise custom. Usage = LLM at provider list price + **flat 40% service fee** + compute; pooled across the team; top-ups pay-as-you-go. Enterprise adds SSO/OIDC/SCIM, CMEK, ISO 42001, SOC 2 reports. "No AI training" on all paid plans. (Pricing has churned repeatedly — see weaknesses.)

### Core design philosophy
- Grounding is the moat: the Context Engine "builds a live understanding of your code, dependencies, architecture, language patterns, and history" ([tembo.io summary](https://www.tembo.io/blog/codegen-alternatives)); marketing claims "1M+ Files Indexed" ([tembo.io](https://www.tembo.io/blog/jules-alternatives)).
- Recent company essays signal a control-plane worldview: "Agents can now run the full SDLC in Cosmos. What do engineers do?", "The bottleneck moved to verification. So we automated that too.", "Introducing Augment Prism: model routing to reduce cost and maintain quality", "The era of single-model engineering is over" ([Augment blog](https://www.augmentcode.com/blog)). Also unusually cost-competitive public benchmarking: "How we rebuilt the Auggie CLI harness to make tasks 53% cheaper than Claude Code", "Opus 4.7 for 33% less: How Auggie beats Claude Code on cost and quality."

### Feature inventory
- **Context Engine**: cloud-hosted semantic + dependency-graph index; claimed scale 400K–500K+ files with cross-repository dependency mapping ([ONES](https://ones.com/blog/solution-guide/enterprise-ai-coding-tools-2026/), [Bito](https://bito.ai/ai-tools/best-developer-productivity-tools/)).
- **Indexing controls**: automatic upload of workspace on open; `.gitignore` + `.augmentignore` (supports `!` to re-include gitignored paths, e.g. `node_modules` for dependency awareness); "proof-of-possession API" and internal data minimization per [workspace-indexing docs](https://docs.augmentcode.com/setup-augment/workspace-indexing).
- **Cosmos**: trigger/schedule-based specialized agents (Code, Review, custom experts) — the "software factory."
- **Code Review**: GitHub PR review on all plans; Enterprise adds analytics dashboards, user allowlists, MCP config (Jira/Linear/Notion context), multi-org, unlimited seats/repos.
- **Auggie CLI**: agent + context engine in terminal; reported 51.8% on SWE-bench Pro, slightly ahead of Cursor and Claude Code on same underlying models ([Bito](https://bito.ai/ai-tools/best-developer-productivity-tools/) — vendor-adjacent, treat with care).
- **Prism**: model routing across providers for cost/quality.
- **Context Engine as a product**: MCP server and SDK let other agents (e.g. Claude Code) query Augment's index; `context-connectors` (MIT) adds incremental updates, S3/filesystem stores, webhook-triggered re-indexing via GitHub Actions/webhooks.

### Architecture (publicly known)
- Code is uploaded to Augment's cloud and indexed there; the engine is proprietary. Retrieval exposed as MCP tools (`search`, `list_files`, `read_file` — `--search-only` mode exists). `context-connectors` architecture: Sources → Indexer (orchestrates Augment's context engine) → Stores (filesystem/S3) → Clients (CLI/agent/MCP) ([GitHub](https://github.com/augmentcode/context-connectors)).
- Enterprise: CMEK (customer-managed encryption keys), SOC 2 Type II, ISO 42001.

### Strengths
- Genuinely best-in-class large-codebase grounding; the recurring user refrain is "context understanding" ([prospeo review tracker](https://prospeo.io/s/augment-pricing-reviews-pros-and-cons): Gartner Peer Insights 4.8/5 from 40 ratings).
- Low adoption friction — lives in the existing editor; published pricing is unusual for an enterprise vendor ([AI Agent Square](https://aiagentsquare.com/agents/augment-code)).
- Aggressive, public cost-engineering culture (harness cost posts) — rare among agent vendors.

### Weaknesses
- **Code must leave the network.** Indexing requires upload to Augment's cloud; no self-hosted Context Engine (only CMEK mitigation). Hard blocker for the privacy-sensitive segment Tabby/Qodo/Greptile-self-host chase.
- **Pricing churn and opacity backlash**: the Oct 2025 credit-based change drew a "disappointment" storm ([r/AugmentCodeAI](https://www.reddit.com/r/AugmentCodeAI/comments/1nzse07/augment_codes_new_pricing_is_a_disappointment/)); G2 shows 2.8/5 (tiny sample, pricing-driven 1-star) vs Gartner 4.8/5 — "a genuine technical edge undermined by a pricing model that feels adversarial" ([prospeo](https://prospeo.io/s/augment-pricing-reviews-pros-and-cons)). The flat-rate 2026 relaunch is the third model in ~18 months — treat current numbers as unstable.
- Context advantage is muted on small/greenfield repos; the 40% LLM markup makes heavy users price-sensitive vs direct API agents.

---

## 2. Sourcegraph Cody

### What it is
- Maker: Sourcegraph (code-search pioneer, founded 2013). Cody is the AI assistant layered on the Sourcegraph code-intelligence platform.
- **Enterprise-only since mid-2025**: Cody Free signups closed 2025-06-25, Free/Pro fully discontinued 2025-07-23; the individual-developer bet was spun out as **Amp** (independent company by end of 2025) ([PromptQuorum](https://www.promptquorum.com/power-local-llm/sourcegraph-cody-review), [whichcoding.tools](https://whichcoding.tools/tools/cody)).
- Target users: large orgs with hundreds/thousands of repos and legacy code; buyers who need compliance + permission-aware context.
- Pricing: Cody Enterprise only — reported ~$49–59/user/mo annual; platform bundles from ~$16K/yr with AI credits ([AICentralResources](https://www.aicentralresources.com/compare/sourcegraph-cody-vs-augment-code), [VibeCompare](https://vibecompare.dev/compare/sourcegraph-cody-vs-github-copilot/) — secondary sources, verify with sales).

### Core design philosophy
- Context over model: "Cody's ability to provide context-aware code responses is what sets it apart... Cody relies on its ability to retrieve context from user codebases" ([Cody context docs](https://sourcegraph.com/docs/cody/core-concepts/context)).
- The strategic pivot itself is a philosophy statement: abandon the consumer seat war (Copilot/Cursor), monetize where codebase scale and permissions make context retrieval a defensible enterprise product.

### Feature inventory
- **Context sources** ([docs](https://sourcegraph.com/docs/cody/core-concepts/context)): Keyword Search (with automatic query rewriting), **Sourcegraph Search API** (the native search stack, managed or self-hosted instance), and **Code Graph** (structural analysis of how components interconnect — SCIP/LSIF-derived precise code intelligence).
- **Permission-aware indexing**: the index preserves code-host ACLs; retrieval is scoped to what the asking developer may see ([AI Toolbox review](https://aitoolboxsolutions.com/tool/sourcegraph-cody)).
- Repo-based multi-repo context in all clients; @-mentions for files, symbols, web URLs, remote files/dirs, OpenCtx (varies by client: VS Code, JetBrains, Visual Studio, Cody Web).
- Admin-configurable context window size; multi-model, admin-selectable LLMs (incl. via BYO/gateway arrangements); up to 1M-token context advertised ([Augment comparison page](https://www.augmentcode.com/tools/sourcegraph-cody-vs-qodo)).
- Platform products: Code Search, Batch Changes, Code Insights, Deep Search; **Sourcegraph MCP server** as the agent-facing surface for third-party agents ([Symvanta](https://symvanta.com/compare/sourcegraph-cody-alternative)).

### Architecture
- Two-tier: the Sourcegraph instance (self-hosted or managed) indexes repos, symbols, branches, relationships; Cody clients send user request + selected context to the instance; search runs server-side; relevant documents return to the IDE and are assembled into the prompt to an approved LLM ([AI Toolbox](https://aitoolboxsolutions.com/tool/sourcegraph-cody), [docs](https://sourcegraph.com/docs/cody/core-concepts/context)).
- Notably: retrieval is **search/keyword + code-graph centric**, not pure vector RAG — embeddings, historically part of Cody context (2023–24), were de-emphasized in favor of Sourcegraph Search + precise code intelligence (the docs list no embedding source; treat exact deprecation timeline as uncertain).

### Strengths
- Unmatched at "big code": decade-old multi-repo estates, 500-microservice sprawl ([DevApps](https://devapps.uk/reviews/sourcegraph-cody-in-2026-the-ai-assistant-for-big-code-problems/)); complementary to Claude Code rather than competitive ([cc.bruniaux.com](https://cc.bruniaux.com/compare/claude-code-vs-cody/)).
- Permission-aware retrieval is a genuine, hard-to-copy enterprise feature — most agents index whatever the local user can see, which breaks in shared/monorepo service deployments.
- Self-hosted deployment option satisfies regulated buyers.

### Weaknesses
- **Consumer abandonment burned goodwill**; price ($49–59/seat enterprise-only) makes it unbuyable for small teams ([AI Agent Square](https://aiagentsquare.com/agents/sourcegraph-cody)).
- Cody is a chat/assist layer, not a frontier agentic executor — teams pair it with Claude Code or Cursor for actual task execution; Amp's spin-out acknowledges Cody lost the agent race.
- Value collapses without buying into the whole Sourcegraph platform (indexing infra, admin overhead).

---

## 3. Greptile

### What it is
- Maker: Greptile (founded 2023 by Soohoon Choi, Daksh Gupta, Vaishant Kameswaran; YC W24).
- Two products on one index: **codebase-understanding API** (natural-language queries over an indexed repo — "How does billing work?") and **automatic PR review with full-codebase context**.
- Pricing ([greptile.com/pricing](https://www.greptile.com/pricing)): Starter free (50 credits/mo, 1 dev); **Pro $30/seat/mo** (50 credits/seat, $1/extra credit; base review = 1 credit, Plus = 3, Apex = 10); Enterprise custom (self-host, SSO/SAML, GitHub Enterprise data residency, GitLab Self-Managed, Bitbucket DC, **Gitea and Perforce**, custom DPA). Free for MIT/Apache OSS; 50% off pre-Series A startups. SOC 2 Type II, HIPAA, GDPR ([aicurated](https://aicurated.io/tools/greptile/)).
- Distribution: GitHub App / GitLab token + webhook; API; VS Code extension; CLI; Slack/Linear integrations; MCP.

### Core design philosophy
- "Unlike traditional linters that check files in isolation, Greptile builds a graph of your entire repository to understand how changes affect the whole system" ([docs intro](https://www.greptile.com/docs/introduction)). Indexing-first: review quality is a retrieval problem.
- Close the loop: every comment has a **"Fix with your Agent"** button shipping file paths, line numbers, and suggested code to Claude Code, Codex, Conductor, Cursor, or Devin; a "Fix All" button sends everything at once ([docs intro](https://www.greptile.com/docs/introduction)).
- Noise as a learning problem: "Your 👍/👎 reactions and replies teach Greptile what matters. After 2–3 weeks, it stops commenting on things you don't care about."

### Feature inventory
- Full-repo code graph (functions, classes, call/import edges); per-PR graph construction for downstream-impact analysis ([Qodo's comparison](https://www.qodo.ai/blog/greptile-vs-qodo/)).
- Swarm-of-agents review: multiple agents review each PR against the graph ([Levelop](https://levelop.dev/blog/best-ai-code-review-tools-2026-coderabbit-greptile-qodo-compared)); reviews land in ~3 minutes; confidence scores on every comment with filter/sort ([RocketContent](https://baeseokjae.github.io/posts/greptile-review-2026/)).
- Review tiers (Base/Plus/Apex) metered in credits — depth is a priced dial.
- Custom rules engine, architecture diagrams in reviews, draft-PR reviews, `@greptileai` manual invocation ([topreviewed](https://topreviewed.ai/products/greptile)).
- API-first: REST API for querying a whole repo in one call; used to build custom internal tooling ([lowcode.agency](https://www.lowcode.agency/blog/claude-code-vs-greptile)).
- "Partners" program: identifies PRs touching supported third-party APIs/SDKs and injects vendor docs, implementation rules, and common failure patterns ([greptile.com/partners](https://www.greptile.com/partners)).
- Greptile Agent v4 (2026-03-05): +74% addressed comments per PR ([stork.ai](https://www.stork.ai/en/greptile)); reported 80% precision on a public review leaderboard as of 2026-08-31 ([officebook](https://officebook.dev/reviews/coderabbit/)).

### Architecture (unusually well documented)
- [System architecture docs](https://www.greptile.com/docs/system-architecture): Docker Compose (single VM, ≤100 devs) or Kubernetes/Helm (100+). Services: `greptile-web` (3000), `greptile-api` (3001), `greptile-auth`, `greptile-webhook` (3007), `saml-jackson` (5225), background workers `indexer-chunker`, `indexer-summarizer`, `reviews`, `jobs`, and **`greptile-llmproxy`** routing to configured LLM providers. Orchestration via **Hatchet** (workflow engine, with its own Postgres + RabbitMQ). Storage: PostgreSQL + **pgvector** ("Code embeddings are the largest storage component"). Data flow: webhook → Hatchet job → chunker/summarizer/reviews workers via llmproxy → results in Postgres → posted back to SCM. Outbound LLM API access required unless air-gapped with custom models. K8s prod sizing: api ×20, summarizer ×50, reviews ×36 — the fleet is LLM-bound.

### Strengths
- Catches cross-file bugs diff-only reviewers miss; reviewers' anecdote of it catching a bug "that required understanding code far outside the diff" ([agentrank](https://www.agentrank.tech/blog/greptile-review-codebase-aware-pr-reviewer)).
- Highest single-pass bug catch rate (~82%) among review tools per [Levelop](https://levelop.dev/blog/best-ai-code-review-tools-2026-coderabbit-greptile-qodo-compared).
- Fix-with-your-agent handoff is the right 2026 posture: review bot detects, the user's own coding agent fixes — no duplicated fixer agent to maintain.
- Broadest SCM coverage in the category at enterprise tier (incl. Perforce + Gitea — genuinely rare).

### Weaknesses
- **More false positives than CodeRabbit**: one controlled comparison logged ~11 FPs vs CodeRabbit's ~2 ([tech-insider](https://tech-insider.org/au/coderabbit-vs-greptile-vs-copilot-2026/), [vantaige](https://vantaige.io/ai-tool/greptile)) — full-repo context buys recall at the cost of noise; the learning loop takes weeks to converge.
- Credit metering punishes high-PR-volume teams: Apex reviews at 10 credits each with 50 credits/seat evaporate fast for teams pushing many daily PRs ([weavai](https://weavai.app/blog/en/2026/05/12/greptile-2026-review-ai-code-review-pricing-debate/)).
- Per-PR graph rebuild cost; historically GitHub-centric (Bitbucket/Azure DevOps support arrived late).
- API rate limits (100 req/hr per IP in HTTP/JSON-RPC mode) constrain custom-tooling ambitions ([stork.ai](https://www.stork.ai/en/greptile)).

---

## 4. CodeRabbit

### What it is
- Maker: CodeRabbit (founded 2023 by Harjot Gill). The market-share leader in AI PR review: 2M+ repos connected, 13M+ PRs reviewed, 8,000+ paying customers by late 2025 ([knowara](https://knowara.com/ai-tools/coding/coderabbit-review/), YipitData via [vantaige](https://vantaige.io/ai-tool/greptile)).
- Surfaces: GitHub App, GitLab, **Bitbucket, Azure DevOps** (only major reviewer covering all four), CLI, IDE; chat in PR threads; MCP connections; Jira/Linear.
- Pricing ([coderabbit.ai/pricing](https://www.coderabbit.ai/pricing), 2026-09): Essentials **$24/dev/mo**, Team **$48**, Advanced **$72** (annual); free forever for public OSS; 14-day trial. Enterprise custom: RBAC/SSO/audit logs, API access, **self-hosting option**, EU SaaS, multi-org, marketplace billing.
- Compliance posture: contractual no-training guarantee, zero-data-retention agreements with OpenAI Enterprise/Anthropic commercial APIs, prompt sandboxing ([humantestsai](https://humantestsai.net/tools/coderabbit/)).

### Core design philosophy
- Pragmatic breadth over depth: diff + file context (not a persistent full-repo dependency graph — "lower-noise but shallower" vs Greptile per [vantaige](https://vantaige.io/ai-tool/greptile)) delivered everywhere the enterprise's code lives.
- Everything is configurable in-repo: `.coderabbit.yaml`, review profiles, path filters/instructions, learnings from replies.

### Feature inventory
- Line-by-line PR review with one-click GitHub suggestion blocks; high-level summary + sequence diagrams (praised as best-in-class for grasping big PRs quickly — [LabHub](https://labhub.hopto.org/blog/culture/2026-05-16-code-review-automation-2026-graphite-aviator-mergify-greptile-coderabbit-copilot-deep-dive?lang=en)).
- Interactive chat on any comment (follow-up analysis); **Learnings** (reply "we do this intentionally because…" and it creates a persistent rule).
- Review profiles: `chill` / `assertive`, plus a **`quiet` profile shipped 2026-07-02** aimed squarely at the noise complaint ([officebook](https://officebook.dev/reviews/coderabbit/)).
- `.coderabbit.yaml`: `path_filters` (exclude lockfiles/build output), `path_instructions` ("in API code, focus on input validation and auth"), global instructions (keep ≤10–12 — more causes noise, per [dev.to guide](https://dev.to/rahulxsingh/coderabbit-custom-rules-review-instructions-guide-1nc1)).
- Linters + SAST tool orchestration inside the review; Triage; Change Stack; Continuous Security Monitoring; tier-gated extras: Team adds multi-repo analysis, custom pre-merge checks, finishing touches (unit tests, merge-conflict resolution); Advanced adds blast-radius/architectural impact analysis and per-PR security review ([pricing](https://www.coderabbit.ai/pricing)).
- "Loops with coding agents" — review comments routed to agent workflows, converging with Greptile's fix-with-agent pattern.

### Strengths
- **Platform coverage**: GitHub + GitLab + Bitbucket + Azure DevOps is the deciding factor in heterogeneous enterprises ([Morph](https://www.morphllm.com/comparisons/coderabbit-vs-copilot)).
- Speed: first review in 60–90s for typical PRs ([aitoolbox.hk](https://www.aitoolbox.hk/articles/best-ai-code-review-tools-2026-coderabbit-vs-github-copilot-vs-amazon-q-vs-codescene/)); small PRs 2–3 min, very large 12–15+ min ([lobehub runbook](https://lobehub.com/skills/jeremylongshore-claude-code-plugins-plus-skills-coderabbit-incident-runbook)).
- Precision over recall vs Greptile: ~2 false positives vs ~11 in one controlled test ([tech-insider](https://tech-insider.org/au/coderabbit-vs-greptile-vs-copilot-2026/)); 52.5% recall vs Copilot code review's 56.5% precision framing ([Morph](https://www.morphllm.com/comparisons/coderabbit-vs-copilot)) — note benchmark sources disagree and are partially vendor-funded.
- No seat-license creep into adjacent products; transparent per-dev pricing.

### Weaknesses
- **Noise is the #1 recurring complaint.** "Lots of trivial comments" ([r/coderabbit](https://www.reddit.com/r/coderabbit/comments/1qok4ca/lots_of_trivial_comments/)); ~28% of comments "noise or based on wrong assumptions" in one analysis ([sfailabs](https://sfailabs.com/guides/cubic-dev-vs-coderabbit)); 47 comments on a single PR under the default assertive profile — "I feel personally attacked by a bot" ([eltexsoft](https://eltexsoft.com/course/coderabbit/)). Default configuration ships noisy; tuning is a first-week tax.
- **Shallow context by design**: no persistent repo graph means it misses cross-file/cross-service breakage that Greptile/Augment catch; multi-repo analysis only on the $48 tier.
- Per-seat pricing gets expensive vs usage-priced competitors (Cursor BugBot moved to ~$1–1.50/run usage billing in June 2026 — [officebook](https://officebook.dev/reviews/coderabbit/)).
- Interactive features (chat, learnings) create lock-in-by-config: a well-tuned `.coderabbit.yaml` is real switching cost, which also means a badly tuned one is churn fuel.

---

## 5. Tabby

### What it is
- Maker: TabbyML. **Open-source (Apache-2.0 core; `ee/` directory under a separate license), self-hosted AI coding assistant** — the canonical "self-hosted Copilot" for privacy-sensitive orgs. ~23K+ GitHub stars ([localaimaster](https://localaimaster.com/blog/tabby-self-hosted-copilot-setup)).
- Surfaces: self-hosted server (Docker, consumer GPU; single binary, no DBMS/cloud deps, no external calls), IDE extensions (VS Code, JetBrains, Vim/Neovim), web admin UI, OpenAPI REST surface (`/swagger-ui`, default port 8080) ([GitHub](https://github.com/TabbyML/tabby), [api-evangelist mirror](https://github.com/api-evangelist/tabby-ml)).
- Distribution: free self-host (enterprise features — codebase indexing, SSO, team management — included in the free version, not upsold, per [AI Agent Square](https://aiagentsquare.com/agents/tabby)); a separate agentic product line **Pochi** ($20 free monthly credits, GitHub-issues-to-PR flow, Dec 2025) is where the company's agent ambitions moved.

### Core design philosophy
- Data sovereignty first: code never leaves your infrastructure — "decisive for regulated, security-conscious, or IP-sensitive teams" ([AI Agent Square](https://aiagentsquare.com/agents/tabby)).
- Model flexibility as strategy: run open models (StarCoder, CodeLlama, Qwen, DeepSeek, Codestral) and swap as the open ecosystem improves; betting on open-model commoditization ([Tabby blog](https://tabby.tabbyml.com/blog/tags/tech-design/)).
- Evaluation honesty: Tabby built its own completion leaderboard using **next-line accuracy** (from RepoCoder/RepoBench/CCEval) because HumanEval is contaminated and LeetCode-style tasks don't resemble real engineering — "metrics that best resemble real-world development workflow," with cross-file context via **BM25** ([Cracking the Coding Evaluation](https://tabby.tabbyml.com/blog/tags/tech-design/)).

### Feature inventory
- Real-time inline completion with **repository-level context (RAG since v0.3.0)**: Tree-sitter query-based code index + BM25 cross-file retrieval + locally relevant snippets (local LSP declarations, recently modified code) ([blog](https://tabby.tabbyml.com/blog/tags/tech-design/), GitHub README history).
- **Answer Engine** (v0.13+): central knowledge engine for internal engineering teams; ingests internal docs via REST API (v0.29); **indexes GitLab MRs as context (v0.30, July 2025)**; switchable chat backends; shareable persistent **Pages** (v0.28).
- Chat with @-mention file context, inline edit, auto commit messages, multiple completion choices.
- Enterprise admin: LDAP auth, GitHub/GitLab (incl. self-hosted) integration + SSO, team management, usage Reports/analytics, storage stats, background job notifications, code browser.
- Model registry: StarCoder, CodeGemma, CodeQwen, DeepSeek, Codestral, etc.; llamafile deployment; SkyServe/SkyPilot cloud deploy; Apple Metal inference.
- Agent line: **Pochi** (VS Code agent, GitHub issues → PRs with CI/lint/test breakdown).

### Architecture
- Rust server; llama.cpp-style local inference (GGUF), CUDA/Metal/CPU; embedded storage (SQLite for admin; built-in index) — "self-contained, needs no external DBMS or cloud service" ([GitHub](https://github.com/TabbyML/tabby)).
- Context pipeline: Tree-sitter for code indexing/symbol extraction; BM25 for cross-file retrieval into completion prompts — deliberately **lexical retrieval, not vector embeddings**, both for product context and for their eval harness.

### Strengths
- The only credible fully-offline answer; genuinely free with enterprise features included ([AI Agent Square](https://aiagentsquare.com/agents/tabby)).
- Deployment simplicity: one Docker command on a consumer GPU; no k8s zoo, no vector DB, no external calls.
- Transparent, research-literate engineering culture (own evals, public tech-design posts).

### Weaknesses
- **Quality ceiling is the local model**: StarCoder-1B-class completions are far behind frontier-model copilots; running bigger open models needs serious GPU ops investment most orgs underestimate.
- **Development momentum risk**: core Tabby releases visibly slowed after v0.30 (2025-07) as the team pivoted to Pochi/agents — the changelog cadence on [GitHub](https://github.com/TabbyML/tabby) is the tell. Self-host buyers carry the maintenance-fork risk.
- No agentic/PR-review story in core Tabby; it is completion + chat + answer engine only.
- "Free" hides TCO: GPU capacity planning per seat is real ops work.

---

## 6. Where PR-review bots add vs destroy value (false-positive economics)

- **The economics**: a review comment costs a human 1–5 minutes of attention. At ~28% noise ([sfailabs on CodeRabbit](https://sfailabs.com/guides/cubic-dev-vs-coderabbit)) or ~11 FPs/run ([Greptile vs CodeRabbit test](https://tech-insider.org/au/coderabbit-vs-greptile-vs-copilot-2026/)), a verbose bot burns more reviewer-hours than it saves, and teams mute it — after which even true positives die. Bots destroy value through **alert fatigue**, not wrong answers.
- **What works, empirically converged across vendors**:
  1. Ship a low-noise default profile (CodeRabbit's `quiet` profile, July 2026; "set quiet before install, not after the first noisy week" — [officebook](https://officebook.dev/reviews/coderabbit/)).
  2. **Confidence scores on every comment** so users can filter (Greptile).
  3. **Learn from 👍/👎 and replies**; reach steady-state suppression in ~2–3 weeks (Greptile; CodeRabbit Learnings).
  4. Path-scoped rules, not global walls of text (≤10–12 global instructions — [dev.to](https://dev.to/rahulxsingh/coderabbit-custom-rules-review-instructions-guide-1nc1)).
  5. Exclude generated code/lockfiles by default.
  6. **Separate detection from fixing**: "Fix with your Agent" (Greptile) and "Loops with coding agents" (CodeRabbit) hand fixes to the developer's own agent — the review bot stays read-only authority.
  7. Make the bot a required-but-fast check: 60–90s first review vs 2–5 min for Copilot ([aitoolbox.hk](https://www.aitoolbox.hk/articles/best-ai-code-review-tools-2026-coderabbit-vs-github-copilot-vs-amazon-q-vs-codescene/)).
- **Recall/precision positioning**: Greptile bets recall (full-repo graph, ~82% catch rate, more FPs); CodeRabbit bets precision (diff+file context, fewer FPs, misses cross-service bugs). Teams pick by noise tolerance; the market has not resolved this — it's a per-team preference dial, not a winner-take-all axis.

---

## 7. What self-hosted buyers demand (checklist distilled from all five)

- No code, embeddings, or file paths leaving the network (Cursor's 2026 local-index posture; Tabby's whole pitch). Augment's cloud-upload requirement is its single biggest enterprise blocker.
- SSO/SAML/OIDC + SCIM, LDAP (Tabby), RBAC, audit logs (CodeRabbit/Greptile enterprise tiers).
- Air-gap deploys with custom/BYO LLM endpoints (Greptile's `llmproxy` + air-gap docs; Qodo's air-gap marketing; Tabby offline-by-default).
- Deployable as Docker Compose for ≤100 devs, Helm for bigger (Greptile's documented split is the industry norm).
- Compliance artifacts even for on-prem: SOC 2 Type II of the vendor, custom DPA, vendor security review support.
- **Permission-aware retrieval** when the index is shared (Sourcegraph's signature feature): an index served to many developers must respect code-host ACLs.
- Incremental re-indexing + CI/webhook-triggered updates (Augment context-connectors ships this as a GitHub Action).
- Cost predictability: the market is punishing per-seat creep and opaque credits — Augment's churn and BugBot's shift to usage pricing both signal buyers now audit agent spend monthly.

---

## 8. Distinctive ideas worth borrowing

1. **Fix-with-your-Agent handoff (Greptile)**. Every review comment carries a machine-readable payload (file paths, line numbers, suggested code) addressed to *any* external agent (Claude Code, Cursor, Codex, Devin). Reimplement as: review findings are emitted as structured artifacts (JSON + markdown) with a copy/click affordance per target agent — detection and remediation stay decoupled, and loongagent can be both the detector and the recipient.
2. **Confidence-scored, learnable review comments (Greptile + CodeRabbit)**. Every comment gets a confidence score; 👍/👎 and replies become persistent per-repo "learnings" that suppress future noise, converging in ~2–3 weeks. Store learnings in-repo (like `.coderabbit.yaml`) so they're versioned and diffable, not hidden in a SaaS dashboard.
3. **Priced review-depth tiers (Greptile credits)**. Base/Plus/Apex reviews at 1/3/10 credits make depth an explicit budget dial. For loongagent's cost governance: a review (or any agent task) should expose effort tiers with known price/depth tradeoffs.
4. **Context Engine as an MCP/SDK product (Augment)**. The index is a standalone API — CLI, interactive agent, and MCP server (stdio/HTTP) over local or S3 stores, with incremental updates and webhook re-indexing ([context-connectors](https://github.com/augmentcode/context-connectors)). Any agent (yours or the user's) can query it. Context infrastructure unbundled from the agent is a durable enterprise surface.
5. **Permission-aware indexing (Sourcegraph)**. The index mirrors code-host ACLs, so shared/monorepo deployments don't leak code across teams via AI answers. Almost no agent does this; enterprises with one shared context service need it.
6. **LLM proxy as a first-class service (Greptile `greptile-llmproxy`)**. All LLM traffic routes through one internal proxy: swap providers, enforce zero-retention agreements, support air-gap custom models, meter spend in one place. This is the thin end of a cost-governance control plane.
7. **Local-only search index as the privacy default (Cursor Instant Grep)**. Build/query the lexical index on the user's machine; upload nothing for search. Embeddings, if used at all, become an opt-in enhancement for huge repos rather than the default pipeline — matching Cursor's measured +12.5% accuracy hybrid result without the data-egress cost.

---

## 9. Lessons for loongagent

**Copy**
- **Hybrid retrieval with structural primacy**: grep/symbol/AST-graph first (fails loudly, respects code structure), embeddings as an optional recall layer for 10M+ LOC estates — and measure the delta the way Cursor did (A/B on code retention + follow-up dissatisfaction, not vibes).
- **Unbundle the context engine**: ship indexing + retrieval as a standalone MCP/SDK service with incremental updates, webhook/CI triggers, and pluggable stores. Enterprises will point other agents at it; that is stickiness, not leakage.
- **Review bot defaults optimized for silence**: quiet-by-default profile, confidence scores, in-repo learnings file, path-scoped rules, generated-code exclusion. Budget the "noise tax" explicitly: every 10 extra comments/PR ≈ 20–50 human-minutes.
- **Detection/fixing decoupling**: review emits structured, agent-consumable findings; loongagent both produces and consumes them.
- **One LLM proxy/control plane**: provider routing, zero-retention enforcement, spend metering, air-gap custom endpoints — this is where loongagent's cost-governance vision plugs into enterprise reality.
- **Pooled flat pricing with metered usage** (Augment's 2026 model) beats per-seat for agent products where usage is wildly asymmetric across developers.

**Avoid**
- Augment's pricing churn (three models in ~18 months destroyed goodwill despite a loved product).
- CodeRabbit's noisy-by-default onboarding (their fix came 2.5 years in).
- Sourcegraph's consumer-abandonment whiplash and platform lock-in — Cody without the Sourcegraph platform is nothing; don't make context features require buying the whole control plane on day one.
- Cloud-only indexing (Augment) for any tier that wants regulated buyers; offer local-index and self-hosted-index paths from the start.
- Tabby's trap: self-hosting purity at the cost of a quality ceiling and (ultimately) product momentum. Self-host must be a deployment option, not the identity.

**Do differently**
- Nobody has unified **permission-aware context + agent execution + review + cost governance** in one plane. Sourcegraph has permissions but lost the agent; Augment has context+agents but no self-host; Greptile has review+self-host but no coding agent; Tabby has self-host but no agent. loongagent's opening: one control plane where indexing (local or server), retrieval policy (ACL-aware), review bots (quiet-by-default, learning), and agent spend are administered together.

---

## Source list (primary)

- Augment: [docs intro](https://docs.augmentcode.com/introduction) · [workspace indexing](https://docs.augmentcode.com/setup-augment/workspace-indexing) · [pricing](https://www.augmentcode.com/pricing) · [blog](https://www.augmentcode.com/blog) · [context-connectors](https://github.com/augmentcode/context-connectors)
- Sourcegraph: [Cody context docs](https://sourcegraph.com/docs/cody/core-concepts/context)
- Greptile: [docs intro](https://www.greptile.com/docs/introduction) · [system architecture](https://www.greptile.com/docs/system-architecture) · [pricing](https://www.greptile.com/pricing) · [partners](https://www.greptile.com/partners)
- CodeRabbit: [pricing](https://www.coderabbit.ai/pricing)
- Tabby: [GitHub](https://github.com/TabbyML/tabby) · [tech-design blog](https://tabby.tabbyml.com/blog/tags/tech-design/)
- Retrieval debate: [Cursor semantic search](https://cursor.com/blog/semsearch) · [Cursor search docs](https://cursor.com/docs/agent/tools/search) · [Cursor forum: indexing settings removed](https://forum.cursor.com/t/what-do-you-think-about-cursor-removing-the-codebase-indexing-settings/165899) · [mindstudio: grep not vectors](https://www.mindstudio.ai/blog/is-rag-dead-what-ai-agents-use-instead)
- Community/benchmarks: [r/AugmentCodeAI pricing](https://www.reddit.com/r/AugmentCodeAI/comments/1nzse07/augment_codes_new_pricing_is_a_disappointment/) · [r/coderabbit trivial comments](https://www.reddit.com/r/coderabbit/comments/1qok4ca/lots_of_trivial_comments/) · [eltexsoft](https://eltexsoft.com/course/coderabbit/) · [sfailabs](https://sfailabs.com/guides/cubic-dev-vs-coderabbit) · [tech-insider 3-way benchmark](https://tech-insider.org/au/coderabbit-vs-greptile-vs-copilot-2026/) · [officebook](https://officebook.dev/reviews/coderabbit/) · [Morph CodeRabbit vs Copilot](https://www.morphllm.com/comparisons/coderabbit-vs-copilot) · [Levelop](https://levelop.dev/blog/best-ai-code-review-tools-2026-coderabbit-greptile-qodo-compared) · [prospeo Augment reviews](https://prospeo.io/s/augment-pricing-reviews-pros-and-cons) · [PromptQuorum Cody](https://www.promptquorum.com/power-local-llm/sourcegraph-cody-review) · [AI Agent Square Tabby](https://aiagentsquare.com/agents/tabby)

*Caveats: Augment's pricing changed repeatedly through 2025–2026; verify current numbers before citing. SWE-bench Pro and review-benchmark figures come from vendor-adjacent or partially vendor-funded sources. Cody Enterprise pricing is not published; figures are third-party estimates.*
