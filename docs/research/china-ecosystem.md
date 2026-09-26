# China Ecosystem: The Chinese AI Coding Tool Market (Trae, Tongyi Lingma/Qoder, CodeBuddy, Comate, iFlyCode, ModelScope)

One-line description: How China's tech giants (ByteDance, Alibaba, Tencent, Baidu, iFlytek) fight for the AI coding market with free-as-weapon pricing, model-vendor bundling, split domestic/international products, and a compliance-first enterprise motion — and what a new globally-ambitious Chinese-built agent should learn from it.

Researched: 2026-09-26

---

## 0. Market snapshot (who's winning)

- **IDC China AI Coding market share report (published 2026-07-16, covering 2025)**: Alibaba leads with **47.6% revenue share** (more than #2–#5 combined), attributed to **Qoder**; then **Zhipu CodeGeeX 11.5%**, **SenseTime Raccoon 10.5%**, **Tencent CodeBuddy ~7%**, **Baidu Comate ~6%**. Market size 2025: **¥399M RMB**, forecast to reach **¥1.173B by end of 2026** — tiny vs the US market but growing ~3x/yr. ([Alibaba Cloud](https://www.aliyun.com/analyst-reports/idc-aicoding-2025), [Xueqiu summary](https://xueqiu.com/S/BABA/400857656), [Eastmoney](https://finance.eastmoney.com/a/202607163808805072.html))
- IDC's framing: AI Coding is moving from "code generation tool" to "software productivity platform" covering requirements → coding → testing → ops. This is the axis every Chinese vendor now markets on. ([IDC abstract](https://my.idc.com/getdoc.jsp?containerId=CHC54607526))
- Adoption gap: Chinese-developer AI coding tool penetration is estimated ~30% vs ~91% for US developers — a large headroom story every vendor pitches. ([Zhihu roundup](https://zhuanlan.zhihu.com/p/2065836878721562607))
- ByteDance's Trae self-reports **6M registered users, 1.6M MAU, ~100B lines of code generated in 2025**, >80% internal ByteDance engineer adoption. (Self-reported; discount accordingly.) ([Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae))
- Adjacent desktop-agent numbers (July 2026, AI产品榜): Tencent **WorkBuddy 11.15M MAU** (+304% MoM), Baidu **DuMate 6.74M MAU** (+1064% MoM) — coding assistants are being folded into broader "desktop agent" products. ([Jiemian](https://www.jiemian.com/article/15064078.html))

---

## 1. What it is — players, surfaces, pricing

### Trae (ByteDance)
- VS Code fork, "The Real AI Engineer"; international version launched **2025-01-19** via Singapore subsidiary Spring (SG) Pte (trae.ai); domestic version launched **2025-03-04** (trae.com.cn). ([Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae), [Toutiao launch note](https://www.toutiao.com/w/1825562685989898/))
- **Two functionally different products under one brand**: international defaults to Claude/GPT/Gemini via Western APIs; domestic runs **Doubao (ByteDance's own LLM) + DeepSeek** on **Volcano Engine** infra. Accounts do not interoperate; international version blocks mainland-China usage. ([Volcengine community](https://developer.volcengine.com/articles/7535310640307830820), [Juejin comparison](https://juejin.cn/post/7477772784098410546))
- Pricing: launched "100% free" as a weapon vs Cursor's $20/mo; **pivoted to token-based billing on 2026-02-24** with five tiers: Free (7-day Pro trial), Lite **$3**, Pro **$10**, Pro+ **$30**, Ultra **$100**/mo. ([aiidelist](https://aiidelist.com/blog/trae-ai-ide-pricing-2026), [Francis Okafor](https://www.francisokafor.com/tools/trae))
- Flagship modes: **Builder** (autonomous scaffolding, ~80% of user engagement) and **SOLO** (full-process autonomous development pipeline, the axis of the new pricing). ([Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae))

### Tongyi Lingma → Qoder CN (Alibaba)
- **Tongyi Lingma**: AI coding assistant from Alibaba Cloud / Tongyi Lab, launched 2023 as IDE plugins (VS Code, JetBrains). Claims >15M plugin downloads, >3B lines generated, >10K enterprise customers, 87% satisfaction (self-reported, May 2025). ([Zhihu/AI早报](https://zhuanlan.zhihu.com/p/1912054229486532032))
- Shipped a standalone **Lingma AI IDE** (2025-05-31), then **Qoder** (agentic coding platform, global launch **2025-08-22**). In 2026 the domestic Lingma line was **rebranded "Qoder CN"** — the Feb 2026 changelog is literally titled "Qoder CN 系列(Lingma)". Qoder CN and Qoder (international) credits are **not interchangeable** — separate pricing, separate metering. ([Alibaba changelog](https://help.aliyun.com/zh/lingma/changelogs-of-202602), [Alibaba Cloud credits doc](https://help.aliyun.com/zh/lingma/credits))
- Lingma pricing tiers: 个人基础版 free; 个人专业版 ~¥59/mo; 企业标准版 ¥79/seat/mo (from 1 seat); 企业专属版 ¥159/seat/mo (100-seat minimum); plus private/VPC deployment. ([企数智 listing](https://www.qishuzhi.com/product/gjdgz7), [Zhihu pricing overview](http://zhuanlan.zhihu.com/p/704690686))
- Qoder (international): credit-based — Free tier, Pro **$30/mo** (2,000 credits), Pro+ $60, Ultra $200, Teams $40/seat (3,000 credits, raised from $30 on 2026-04-30); Enterprise custom with on-prem. Product matrix now includes Qoder IDE, JetBrains plugin, CLI, mobile app, **QoderWork** (desktop agent, Jan 2026), **QoderWake** ("digital employee", Apr 2026), cloud agent runtime. ([ToolCenter Qoder review](https://www.toolcenter.ai/en/articles/qoder-review-2026), [tooldirectory.ai](https://tooldirectory.ai/tools/qoder), [Baidu Baike](https://baike.baidu.com/en/item/Qoder%20IDE/5609315))

### CodeBuddy (Tencent)
- Tencent Cloud's AI coding family: **IDE plugin** (VS Code/JetBrains/WeChat DevTools), standalone **CodeBuddy IDE** (public beta opened Sept 2025, no invite code), **CodeBuddy Code CLI** (`npm install -g @tencent-ai/codebuddy-code`), and **WorkBuddy** (non-developer desktop agent sharing the subscription). ([AIStarMap](https://www.aistarmap.com/en-US/aitool/codebuddy-ide), [Intellectia](https://intellectia.ai/news/stock/tencent-releases-ai-programming-tool-codebuddy-domestic-version-supports-deepseek))
- Dual-model strategy: domestic version integrates **DeepSeek + Tencent Hunyuan**; international (codebuddy.ai) uses GPT/Claude-family models. Overseas registration restricted — reportedly only 4 Asian countries selectable, no US. ([Baidu Baike](https://baike.baidu.com/en/item/CodeBuddy/1432154), [YouTube review summary](https://www.youtube.com/watch?v=U7nESfFST9Y))
- Pricing: personal Pro **¥39/mo (~$5.4)** — about half of Copilot; enterprise pay-per-call with domestic data residency as the selling point. ([Tencent Cloud community](https://cloud.tencent.cn/developer/article/2563818?policyId=1004))

### Baidu Comate (文心快码)
- Baidu's commercial coding assistant on ERNIE models; plugin + standalone **Comate AI IDE** (launched 2025-06, with F2C design-to-code, live preview, proactive requirement-clarification). Agent named **Zulu**. Claims >10M developers served; inside Baidu, AI-generated code surpassed human-written code by end of 2025 (self-reported). ([中国科技网](https://www.stdaily.com/web/gdxw/2025-06/24/content_359265.html), [Jiemian](https://www.jiemian.com/article/15064078.html))
- Pricing criticized early: Zulu agent free quota ~50 requests, then ¥59/mo (list ¥100) — "this price is close to Trae's" was a common Zhihu jab. ([Zhihu](https://zhuanlan.zhihu.com/p/1920770959679414475))
- **2026-09-07: Comate was folded into Baidu DuMate (百度搭子)**, Baidu's general-purpose agent platform, becoming its coding capability module — a signal that standalone coding-tool P&Ls are being absorbed into broader agent bets. ([Jiemian](https://www.jiemian.com/article/15064078.html), [CLS](https://api3.cls.cn/share/article/2475962?os=web&sv=859&app=))

### iFlytek iFlyCode (星火飞码)
- iFlytek's coding assistant on the SparkDesk/星火 model, launched 2023-08; completion/generation/explanation/unit-test/SQL-optimization across 100+ languages, VS Code + JetBrains plugins; SaaS + **private deployment** options, explicitly "adapts to 国产化 (domestic-stack) system environments" — targets government/education/finance where iFlytek's channel is strong. ([会用AI导航](https://www.hyaidh.com/content/category/sites20/79.html), [ai-bio.cn](https://ai-bio.cn/sites/454.html))
- Minor player in developer mindshare; mostly rides iFlytek's existing 政企 (government/enterprise) sales motion.

### ModelScope (魔搭) and free-model bundling
- Alibaba DAMO's model hub (China's Hugging Face) doubles as a **free inference API pool**: after binding an Alibaba Cloud account + real-name verification, users get ~**2,000 free API calls/day** across ~3,000 open models (Qwen, DeepSeek, GLM, MiniMax; DeepSeek-R1-class models capped ~200/day), OpenAI-compatible endpoint at `api-inference.modelscope.cn/v1`. ([mdnice free-API guide](https://mdnice.com/writing/8bbdf08a2d354abc97d73b0bcc05ccbe), [iGetoken guide](https://igetoken.com/models/modelscope/))
- This pool is the default free backend for Chinese CLI agents: when **Qwen Code's OAuth free quota was shut down (2026-04-15)**, users were rerouted to ModelScope keys. ([lanmao.ai](https://lanmao.ai/zh/))
- Companion pattern: **SiliconFlow** (aggregator, 1,000 RPM free per model) and vendor promos (e.g. Huawei Ascend-backed AtomCode, 30-day free DeepSeek/Qwen) make "free frontier-ish model access" a normal market expectation in China. ([Juejin free-API roundup](https://juejin.cn/post/7623066322222874624), [Tencent Cloud article](https://cloud.tencent.com/developer/article/2712144))

---

## 2. Core design philosophy (in the makers' own words)

- **Free-as-weapon / subsidy land-grab.** Trae launched "100% free" explicitly against Cursor's $20; ByteDance's ~**$23B 2026 AI capex** (half on chips) makes the subsidy a rounding error — the real goals are developer mindshare, the Doubao flywheel, and data. Trae is "not a product strategy; it is a distribution capture play." ([Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae))
- **"AI-native development paradigm" (AI 原生研发新范式)** — Alibaba's Lingma tagline: not autocomplete but "code generation + R&D Q&A + autonomous task execution" across the whole lifecycle. ([lingma.aliyun.com](https://lingma.aliyun.com/))
- **"Agentic coding platform for real software"** — Qoder's deliberately narrow launch pitch: two modes (Agent = supervised pair programming with checkpoint review; Quest = spec → autonomous execution → change report). "Not autocomplete. Not a chat sidebar." ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026))
- **Cost-aware multi-model routing as a first-class feature.** Qoder's scheduler picks the cheapest adequate model per request (Claude for reasoning, GPT for general work, Gemini Flash for mechanical edits; 5–10x per-response cost spread). ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026)) CodeBuddy prices models asymmetrically — "Claude costs double credits, GPT nearly free." ([YouTube review](https://www.youtube.com/watch?v=U7nESfFST9Y))
- **Ecosystem-native beats model-native.** Lingma's strategy is "阿里云生态的原住民" — deep integration with Alibaba Cloud, DingTalk, Apache Dubbo/Spring, Nacos/Sentinel; CodeBuddy's is WeChat mini-program knowledge bases and Tencent CloudBase one-click deploy. ([Juejin roundup](https://juejin.cn/post/7627405638161121307), [CodeBuddy docs](https://www.codebuddy.ai/docs/ide/User-guide/Overview), [vibecodinghub](https://vibecodinghub.org/tools/codebuddy))
- **Compliance is the product for enterprise.** "代码、API、训练数据，能不能一滴都不流出去?" ("can not one drop of code/API/training data leak out?") is the #1 enterprise question post-2024 生成式AI管理办法; private deployment is the default opening demand. ([CSDN enterprise deployment guide](https://blog.csdn.net/weixin_33769125/article/details/92048157))

---

## 3. Feature inventory

### Trae
- Builder mode: NL → full project scaffold (React+Express+auth in <2 min reported); multimodal input (screenshot/Figma → UI code); multi-file context; autonomous task decomposition; **11,000+ MCP tool integrations**; terminal integration with permissioned command execution. ([Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae), [aicoolies review](https://aicoolies.com/reviews/trae))
- SOLO mode: full-process autonomous pipeline (plan → code → test → deploy); became the primary billing axis in Feb 2026. Juejin field report: "3 hours to ship a full-stack website incl. deployment." ([Juejin SOLO writeup](http://juejin.cn/post/7547498416759652404))
- Product metrics: completion latency −60% and memory −43% vs launch (self-reported). ([Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae))

### Tongyi Lingma / Qoder CN
- Plugin + IDE + CLI surfaces; **agent mode** (autonomous decisions, environment sensing, tool use: project retrieval, file editing, terminal) added Apr 2025; **long-term memory** (personal/project/problem memories, auto-organized); full **Qwen3** support; international-site login and dedicated edition in Singapore/Malaysia regions. ([Apr 2025 changelog](https://help.aliyun.com/zh/lingma/changelogs-of-202504))
- Feb 2026 (Qoder CN): built-in **planning agent** with human-in-the-loop plan approval before execution. ([Feb 2026 changelog](https://help.aliyun.com/zh/lingma/changelogs-of-202602))
- "AI 程序员" (Jan 2025): first multi-file edit capability; demo: login+data-management web page in 10 min. ([IT之家](https://www.ithome.com/0/823/315.htm))
- Enterprise: 标准版 (accounts, authorization, stats reports, audit logs, 500MB knowledge base) vs 专属版 (SSO/account integration, 5GB enterprise knowledge base, codebase-generation enhancement, **VPC deployment**); admin multi-model configuration letting admins choose which models developers may use. ([Enterprise quickstart](https://help.aliyun.com/zh/lingma/getting-started/enterprise-standard-quick-start), [集成企业API guide](https://volcengine.csdn.net/694e3fa15b9f5f31781b0f37.html))
- Qoder (international) specifics: **Repo Wiki** (structured, human-readable repo index the agent reads before answering), **checkpoints** (diff preview before any file touched — default-on), **Quest change reports** (markdown summary of what/where/tests for faster review), credit metering where Quest tasks cost 80–200 credits and Claude responses 5–10x Gemini Flash. ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026))

### CodeBuddy
- Modes: **Ask** (Q&A, no edits), **Craft** (agent, local multi-file edits), **Plan** (most autonomous: task list → confirm → execute read/modify/env-check). Plus **custom agents** ("+ Create Agent") for business-specific workflows. ([CodeBuddy docs](https://www.codebuddy.ai/docs/ide/User-guide/Overview))
- **NES (Next Edit Suggestions)** predictive editing — predicts the *next* edit location/block, Cursor-Tab-style, default-on, `Ctrl+Shift+Enter` manual trigger. ([CodeBuddy docs](https://www.codebuddy.ai/docs/ide/User-guide/Overview))
- Extensibility: **Memories** (auto-recorded preferences/context), **Rules** (`.codebuddy/rules`), **Skills** (`.codebuddy/skills/` — packaged instruction sets for e.g. security audit, perf testing), MCP support, custom-model API integration. ([CodeBuddy docs](https://www.codebuddy.ai/docs/ide/User-guide/Overview))
- Ecosystem: Figma-to-code, enterprise component-library integration, **CloudBase/Supabase/CloudStudio one-click deploy**, WeChat mini-program knowledge bases. ([CodeBuddy docs](https://www.codebuddy.ai/docs/ide/User-guide/Overview), [vibecodinghub](https://vibecodinghub.org/tools/codebuddy))

### Baidu Comate
- Full-lifecycle agents (plan/code/unit-test/debug/review/Q&A), **multi-agent orchestration**, tool self-invocation, error-log capture; **F2C** (Figma/design-to-code) with live preview and **proactive requirement clarification** (the IDE asks you questions before generating); Comate+ open platform; **Agent Hub** with seven component types (Agent, Plugin, Skill, MCP, Rules, Command, best practices) distilled from Baidu-internal usage; C++ generation quality ranked #1 in an IDC capability eval (vendor-cited). ([Comate site](https://comate.baidu.com/zh), [中国科技网](https://www.stdaily.com/web/gdxw/2025-06/24/content_359265.html), [SegmentFault](https://segmentfault.com/a/1190000047994470))

### Cross-cutting / ecosystem features
- **Free model aggregation as infrastructure**: ModelScope's 2,000-calls/day free pool, SiliconFlow's per-model RPM freebies, and vendor promo credits mean Chinese devs expect a zero-cost path for everyday agent use. ([mdnice](https://mdnice.com/writing/8bbdf08a2d354abc97d73b0bcc05ccbe))
- **Login via super-apps**: CodeBuddy onboarding is WeChat-scan login; Lingma binds to Alibaba Cloud/DingTalk accounts. ([Tencent Cloud community](https://cloud.tencent.cn/developer/article/2563818?policyId=1004))

---

## 4. Architecture (publicly known)

- **Trae** is a confirmed **VS Code fork** that inherits VS Code's telemetry infrastructure and adds ByteDance data pipelines; includes a **remote hot-update mechanism** (ByteDance can modify functionality without visible version bumps). Domestic version runs entirely on **Volcano Engine** with Doubao/DeepSeek, deliberately free of US API dependencies (part of a ~$23B capex hedging strategy). ([Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae), [The Register](https://www.theregister.com/software/2025/07/28/bytedance-ai-ide-trae-telemetry-continues-even-after-opt-out/838532))
- **Qoder**: standalone desktop IDE (not a plugin) integrating Claude/GPT/Gemini behind an **internal cost-aware scheduler**; Agent and Quest modes share repo context (a Quest task can be handed to Agent for follow-up); Repo Wiki is a precomputed structured index. ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026))
- **Lingma**: plugin-first architecture (VS Code/JetBrains) over Qwen models; enterprise dedicated edition adds **VPC deployment**, enterprise RAG knowledge bases (vectorized), codebase-enhanced generation. ([Enterprise docs](https://help.aliyun.com/zh/lingma/enterprise-dedicated-edition-quick-start))
- **CodeBuddy**: plugin + standalone IDE + CLI sharing one account/credit system; NES predictive editing engine; `.codebuddy/` directory convention (rules, skills) mirroring `.cursor/` and `.claude/` conventions. ([CodeBuddy docs](https://www.codebuddy.ai/docs/ide/User-guide/Overview))
- **Comate**: plugin + standalone IDE; enterprise pitch is full-stack private deployment — **model inference + RAG vectorization all inside the customer VPC**. ([Juejin private-deployment comparison](https://juejin.cn/post/7586844323619045391))
- **ModelScope free inference**: OpenAI-compatible gateway (`api-inference.modelscope.cn/v1`) backed by Alibaba Cloud compute; requires real-name-bound Alibaba Cloud account; per-model daily caps. ([iGetoken](https://igetoken.com/models/modelscope/))

---

## 5. Strengths (community-cited)

- **Trae: free frontier models.** "$0 access to Claude/GPT-4o on the free plan — those cost real money everywhere else" is the recurring praise; Builder's screenshot-to-code got genuine love from vibe-coding communities; >80% internal ByteDance adoption is cited as the strongest quality signal. ([YourAIFinder](https://youraifinder.com/blog/best-free-ai-coding-tools-beginners-no-credit-card-2026), [Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae))
- **Qoder: Quest mode actually ships.** "Most 'autonomous' coding tools in 2025 were demos. Qoder's Quest actually ships working code on bounded tasks"; Repo Wiki "genuinely changes how the agent answers"; checkpoint-first defaults "match how senior developers actually want to work"; change reports "underrated — speeds up code review." Chinese testers praise context continuity and multi-file coherence: "很多原本需要来回切窗口、查文件、翻逻辑链的动作，现在直接在 qoderwork 里." ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026), [Sina user report](https://www.sina.cn/news/detail/5297841054419801.html))
- **CodeBuddy: Chinese-language UX + speed + price.** Java-plugin comparative review recommends it for individuals/students: "免费无限制 + 中文注释优化 + 响应快"; toolset "far surpasses Cursor's offerings" incl. integrated Figma-to-code; NES praised as beyond-Cursor predictive completion. ([Sina comparison](https://cj.sina.cn/articles/view/7879848900/1d5acf3c401902t3dy?froms), [Medium review](https://medium.com/@lcxfs1991/in-depth-review-of-tencents-new-codebuddy-ide-a-revolutionary-upgrade-for-developer-experience-465845c4bc63))
- **Lingma: ecosystem fit and enterprise trust.** Deep Alibaba Cloud / DingTalk / Spring / Dubbo integration; private deployment wins finance-sector deals ("某金融公司…所有代码生成和优化操作都在本地服务器完成"); 87% claimed satisfaction. ([Juejin](https://juejin.cn/post/7627405638161121307), [Jingdianlaoge review](https://www.jingdianlaoge.com/ai/HtiR7h23Bllw5SeU6Sqdctyol4aOJSiZZweonl8tg5G.html))
- **Comate: enterprise R&D-asset reuse.** IDC capability eval cited at 8/9 full marks; Agent Hub's codified internal best practices; strong C++ generation. ([SegmentFault](https://segmentfault.com/a/1190000047994470))

---

## 6. Weaknesses (recurring complaints)

- **Trae: trust collapse.** (a) July 2025 telemetry scandal — ~500 network calls / ~26MB in 7 minutes **even with telemetry off**, incl. file paths, hardware, keystroke/focus metrics; HN pile-on ("top-voted comment was some variation of 'called it'"); (b) Feb 2026 pivot broke the "forever free" promise — users report <20 requests/month vs the original 600 fast requests; (c) alleged model misrepresentation (advertised "Claude 4", served 3.5 Sonnet); (d) **queueing even for Pro users at peak** ("Pro 和非 Pro 在排队上没有明显区别" — V2EX, Jan 2026); (e) no SOC 2, GDPR "contested" → structurally excluded from regulated-industry procurement. ([The Register](https://www.theregister.com/software/2025/07/28/bytedance-ai-ide-trae-telemetry-continues-even-after-opt-out/838532), [Cybernews](https://cybernews.com/security/bytedance-ai-coding-tool-trae-data-collection/), [V2EX queue thread](https://hk.v2ex.com/t/1183793), [Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae))
- **Trae: greenfield-only agent.** Builder is ~80% of usage because output is 60–70% usable on new projects but ~40% on complex existing codebases; context loss on large multi-file edits; occasional unintended file overwrites. ([Trae teardown](https://www.openaitoolshub.org/ai-product-research/trae))
- **Trae domestic edition**: "整体体验上不如国际版感受流畅，稳定性也不尽人意" — Doubao/DeepSeek coding quality lags Claude for complex work. ([Juejin](https://juejin.cn/post/7477772784098410546), [Volcengine](https://developer.volcengine.com/articles/7535310640307830820))
- **Qoder: credit anxiety.** "Qoder太贵了，用起来肉疼" — the credit model punishes exploration; power users burn Pro's 2,000 credits in 10–12 days; "随便改一下就是几十分，确实顶不住" (V2EX: "$20 for 2,000 credits, who approved this pricing?"); model identity hidden from users ("它模型不给客户看，20美金我为啥不买 cursor"). Free tier "几乎不能用" — even edit mode queues. ([Qoder forum](https://forum.qoder.com/t/qoder/9465), [V2EX](https://fast.v2ex.com/t/1159185), [xmsumi comparison](https://www.xmsumi.com/detail/3243))
- **Qoder: rough edges.** Mediocre tab completion (behind Cursor/Copilot), 4–7s first-token latency on Claude routes, editor polish/extension gaps, **SSH-remote bugs** (custom models unavailable on remote projects; file upload broken because the agent runtime sits on the remote host), CLI remote-control session chaos (~32 duplicate sessions). ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026), [v2ex.top daily digest 2026-09-23](https://v2ex.top/))
- **Lingma: plugin UX and stagnation perception.** V2EX: forced Tab-key binding conflicts with IDE completion ("我就补全个变量名…直接把它"); Sept 2025 thread "阿里的通义灵码不维护了吗" (slow answers); unit-test generation "Mock 逻辑不完整" in head-to-head tests. ([V2EX 2023](https://www.v2ex.com/t/987757), [V2EX 2025](https://www.v2ex.com/t/1160926), [Sina comparison](https://cj.sina.cn/articles/view/7879848900/1d5acf3c401902t3dy?froms))
- **CodeBuddy international**: registration locked to 4 Asian countries, Hunyuan unavailable overseas, unusual credit asymmetry (Claude 2x credits) confuses users. ([YouTube review](https://www.youtube.com/watch?v=U7nESfFST9Y))
- **Comate**: stingy free tier for Zulu (50 requests) vs Trae/Lingma free — "50次请求能干啥"; absorbed into DuMate suggests standalone traction was insufficient. ([Zhihu](https://zhuanlan.zhihu.com/p/1920770959679414475), [Jiemian](https://www.jiemian.com/article/15064078.html))
- **Community verdict on the whole category** (Chinese dev, Feb 2026): "windsurf >= claude code > augment code > cursor >> trae >> qoder = Lingma" — domestic tools still trail Western frontier tools on pure coding quality; recommended budget path is "Claude Code + GLM" at ~80% of the effect. ([chat2024.cn](https://www.chat2024.cn/2026/02/03/ai-programming/73242/))

---

## 7. Distinctive ideas worth borrowing

1. **Cost-aware model routing as a visible product feature (Qoder).** An internal scheduler picks the cheapest adequate model per request (mechanical edits → Flash-class, reasoning → frontier), with user override ("pin this model"). Reported 5–10x per-response cost spread. Implementable as: classify each request (edit/chat/refactor/plan), map to a model tier table, expose routing decisions + savings in the UI, allow per-project pinning. Directly serves loongagent's cost-governance vision. ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026))
2. **Repo Wiki: precomputed, human-readable project index (Qoder).** Instead of opaque embeddings only, generate a structured markdown wiki of modules/types/entry points/build configs that both the agent reads *and* the human can inspect/edit. Doubles as living documentation and as a debuggable context artifact. ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026))
3. **Quest change reports.** Every autonomous run emits a markdown report (goal, files touched, tests added, deviations) alongside the diff, so reviewers read the report first. Trivially implementable, large review-velocity win. ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026))
4. **Checkpoint-first defaults (Qoder) / Plan-then-confirm (CodeBuddy Plan mode, Lingma planning agent).** Multi-step agent plans pause for diff/plan approval *before* touching files, default-on. The Chinese tools converged on "show me first" as the senior-developer default — the opposite of early Cursor Composer's eager-apply behavior. ([ToolCenter](https://www.toolcenter.ai/en/articles/qoder-review-2026), [CodeBuddy docs](https://www.codebuddy.ai/docs/ide/User-guide/Overview), [Feb 2026 changelog](https://help.aliyun.com/zh/lingma/changelogs-of-202602))
5. **The `.codebuddy/` layered config convention: Rules + Skills + Memories as files.** Rules (architecture/style constraints), Skills (packaged task playbooks like "security audit"), Memories (auto-curated preference/context store) — all as versionable project/user-level files. This is a clean, git-diffable generalization of `.cursorrules` + Claude Code's CLAUDE.md + skills. ([CodeBuddy docs](https://www.codebuddy.ai/docs/ide/User-guide/Overview))
6. **Free model pool as onboarding infrastructure (ModelScope pattern).** Alibaba gives ~2,000 free OpenAI-compatible calls/day to real-name users and lets CLI tools (Qwen Code) point at it — converting a model hub into a top-of-funnel for agent tools. A new agent can bootstrap zero-cost trials by pre-integrating such pools (ModelScope, SiliconFlow) instead of subsidizing inference itself. ([mdnice](https://mdnice.com/writing/8bbdf08a2d354abc97d73b0bcc05ccbe), [lanmao.ai](https://lanmao.ai/zh/))
7. **Enterprise knowledge-base-enhanced generation with admin-controlled model menus (Lingma 专属版).** Admins configure which models developers may select, attach a 5GB enterprise knowledge base + codebase-enhanced generation, all deployable in a customer VPC. The "admin model menu" is a simple, high-value governance primitive almost no Western tool ships. ([Enterprise docs](https://help.aliyun.com/zh/lingma/enterprise-dedicated-edition-quick-start))

---

## 8. Lessons for loongagent

**Copy:**
- **Split governance from execution**: admin model menus, per-seat credit accounting, VPC/private deployment from day one — this is what actually closes Chinese enterprise deals (finance/政企). The enterprise question is "can not one drop leak?", not "is the model smartest?". ([CSDN](https://blog.csdn.net/weixin_33769125/article/details/92048157))
- **Cost governance as UX**: Qoder's routing + visible credit burn per task is the right idea executed opaquely; loongagent can win by making burn *predictable* (pre-flight cost estimates, budget caps per task/team, model-tier tables) — the #1 Qoder complaint is unpredictability, not price level.
- **Checkpoint-first + change reports + repo wiki**: cheap to build, directly attack the "I can't review what the agent did" pain that all Chinese incumbents still handle clumsily.
- **Ecosystem bundling**: Lingma/DingTalk and CodeBuddy/WeChat show distribution comes from embedding where developers already authenticate and deploy. For global ambitions: GitHub/GitLab SSO + one-click deploy targets matter more than another editor fork.
- **Free-pool onboarding**: pre-integrate ModelScope/SiliconFlow-class free tiers so day-one usage costs the user (and you) nothing.

**Avoid:**
- **The Trae trust trap**: never couple "free" growth to telemetry that survives opt-out, remote hot-update channels, or model misrepresentation ("Claude 4" that isn't). One Register headline permanently cost Trae the entire regulated-industry segment — and it is the exact segment loongagent's control-plane story targets. No-SOC-2 + opaque data flows = procurement death in the West. ([The Register](https://www.theregister.com/software/2025/07/28/bytedance-ai-ide-trae-telemetry-continues-even-after-opt-out/838532), [Kaamel privacy teardown](https://www.kaamel.com/blog/en-ai-coding-agent-privacy-due-diligence/))
- **Credit opacity**: Qoder's credit system ("奇葩的credits计算") and Trae's bait-and-switch repricing both generated lasting resentment. If you meter, meter in units users can predict (tokens with visible counters, or flat tiers with honest limits).
- **Greenfield-only agents**: Trae's Builder is 80% of usage because it fails on existing codebases (~40% usable). The durable money is in brownfield repo-level work — budget context engineering accordingly.
- **Two-brand schizophrenia done badly**: Trae CN/Intl and Qoder/Qoder CN splits are regulatory necessities, but non-interoperable accounts/credits and country-locked registration (CodeBuddy) breed confusion. If loongagent must split, make the split explicit, single-account, and feature-flagged rather than forked.

**Do differently (the open wedge):**
- **Be the privacy-certifiable Chinese-built agent.** Every Chinese incumbent is locked out of Western regulated procurement by parentage + telemetry practices; every Western tool is locked out of Chinese enterprises by data-residency law. A vendor-neutral agent with (a) SOC 2 / GDPR-native posture, (b) true local/VPC/air-gapped modes, (c) auditable, toggle-respecting telemetry, and (d) BYOK across both Western and Chinese model providers can credibly serve both markets — none of the incumbents above can.
- **Don't fork VS Code again.** Trae, Cursor, Windsurf, Qoder, CodeBuddy IDE are all VS Code forks competing on the same surface; the unclaimed surface is the **backend control plane** (org-wide policy, cost, audit, agent fleet management across TUI/IDE/web) — exactly loongagent's stated vision.

---

## Appendix: quick reference table

| Product | Maker | Surfaces | Entry price (2026) | Models | Notable |
|---|---|---|---|---|---|
| Trae Intl | ByteDance (Spring SG) | VS Code fork IDE | Free trial / $3–$100 | Claude, GPT, Gemini | 6M reg users; telemetry scandal; token pivot Feb 2026 |
| Trae CN | ByteDance | IDE | Free | Doubao, DeepSeek | Volcano Engine; "China's first AI-native IDE" |
| Tongyi Lingma / Qoder CN | Alibaba Cloud | Plugin, IDE, CLI | Free / ¥59–¥159 seat | Qwen3 | 15M plugin downloads; VPC enterprise edition |
| Qoder | Alibaba | IDE, JB plugin, CLI, mobile, cloud runtime | $30/mo (2k credits) | Claude/GPT/Gemini routed | Quest mode, Repo Wiki, 47.6% CN market share |
| CodeBuddy | Tencent Cloud | Plugin, IDE, CLI, WorkBuddy | Free / ¥39 Pro | Hunyuan + DeepSeek (CN), GPT/Claude (Intl) | NES predictive edit; WeChat ecosystem |
| Comate | Baidu | Plugin, IDE | Free / ¥59 | ERNIE + custom | Merged into DuMate Sept 2026; Agent Hub |
| iFlyCode | iFlytek | Plugin | Free + enterprise | SparkDesk | 政企/government channel; domestic-stack support |
| ModelScope | Alibaba DAMO | Model hub + free inference API | Free (~2,000 calls/day) | Qwen/DeepSeek/GLM/MiniMax | De-facto free backend for CN CLI agents |
