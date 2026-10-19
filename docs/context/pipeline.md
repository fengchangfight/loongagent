# 流水线

Language: zh

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0006](../decisions/0006-context-engine.md) · [ADR-0017](../decisions/0017-layered-context.md)
- **Code:** `crates/context/src/digest.rs`, `crates/context/src/synthesis.rs`, `crates/context/src/worker.rs`
- **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`

## 6. 流水线

### 6.1 统一形状

```
新增事件 → L1 物化 → 索引 → 〔切段〕 → L2 归纳 → 归入模块 → L3 归纳 → L4 归纳
                                  ↑                  ↑
                              确定性 + 模型       确定性 + 模型
```

### 6.2 L2 的两段式：digest（确定性）与 synthesis（模型）

- **digest（R 模式也有）**：把一个区间的 L1 记录压成结构化事实清单 —— 改动的文件、
  运行的命令与结果、错误码、状态变化。零模型，纯模板。它已是强索引。
- **synthesis（仅 S 模式）**：模型读 digest + 少量原文，产出"为什么/结论/被否决方案/
  下一步"。这是唯一必须模型的地方 —— 而且**只让它产出真的需要理解的那几个字段**
  （[principles.md](principles.md) §4.4 的字段归属表）。
- 两者**共存**于同一条 `summary`（`producer` 区分），synthesis 失效时 digest 仍在。

**一条 2026-10-14 修正：`rejected` 由程序算，不再问模型。**

实测（7B，同一份 digest）：
- 提示写"哪些方案被放弃了"→ 模型答 `[]`（它不敢判断）；
- 改成"照抄随后验证失败的那次尝试"→ 有时答对（如 `改 src/cache/mod.rs【只清一层缓存】 → 随后验证失败`），
  有时**编造**一个从未失败过的测试（`测试 pool_swap：failed`）。

而"哪次尝试随后验证失败"**本来就是 harness 手里的事实**（新增的 `Edit` 序列：每次写入 + 紧随其后的验证结果）。
让模型去抄一份它已经能读到的清单，只会引入失败模式。因此：
- digest 里新增 `经过（按顺序，含每次尝试后的验证结果）` 与**由程序算出的** `被否决（随后验证失败）：…`；
- 模型只回答 `why` / `open_questions` / `conclusion` —— schema 里**没有** `rejected` 字段。
  **一个模型能填错的字段，迟早会被填错。**

这也顺带修正了一个更早的诊断：我之前说"0.6B/3B 不够好、建议换 7B"，
其中"3B 编造被否决方案"有一部分是**我的输入没有记录尝试序列**逼出来的。
现在工具链修好了，换模型这个结论需要重新测。

**L2 的字段与形态（digest 与 synthesis 合成之后）**：

```jsonc
{
  "id": "ep-00000118-00000143",  "level": 2,   "scope": "episode:ep-00000118-00000143",
  "title": "修复 config 层叠失效",                 // 程序（取自首个 user 消息），模型不改标题
  "status": "done",                                 // 程序：测试通过 + 有 commit
  "touched_files": ["src/config/merge.rs"],         // 程序：L1 实体
  "commands": ["cargo test layer_merge"],           // 程序
  "tests": [{"name":"layer_merge","result":"failed→ok"}],  // 程序（保留"先失败后通过"）
  "rejected": ["改 src/config/merge.rs（深拷贝后再合并）"],  // ★程序：由 Edit 序列算出
  "open_questions": [],                             // ★模型
  "why": ["层叠顺序不能依赖调用序"],                 // ★模型
  "conclusion": "改为按层号排序后合并，测试通过",     // ★模型
  "citations": [[118,143]],                         // 程序：子层区间
  "producer": "digest+model:qwen2.5:7b-instruct"    // 逐字段来源可查
}
```

**关于上面这张图与实现的两点差别**（2026-10-14）：

- **没有单独的 `degraded` 字段。** 实际存储只有 `producer`：模型失败时它写成
  `digest+model:<name> (failed: <reason>)`，而 `ContextExplain.memory_degraded` 与
  `Store::degraded_synthesis()` 从这串文字把"N 段退回 digest"读出来。字段少一个，
  "这条摘要被模型碰过没有、碰成功了没有"就只有一处真相。
- **`title` 不由模型改写**；digest 与 synthesis 的字段归属见 [principles.md](principles.md) §4.4。
  `producer` 里是**默认模型**（实测已改为 7B，见 [rollout.md](rollout.md) §14 未决项 2）。

**`tests` 保留"失败→通过"这个转变**，而不是只留最终结论 —— 因为"先失败后通过"正是
"这个修复是被验证过的"的唯一证据。

**本机实测（决定 S 模式怎么用 0.6B，见 [rollout.md](rollout.md) §14 未决项 1）**：
- 直接把**原始事件清单**丢给 `qwen3:0.6b`：它会整段复述到 `rejected[]` 里，并且把
  `status` 猜成 `done`/`open` 乱来 —— **完全不可用**。
- 把输入改成**已归纳的事实句**（"做法一：先深拷贝再合并 → 测试仍然失败，已放弃"），
  只问它"哪个做法被否决了、为什么"：`qwen3:0.6b` 一次就答对了
  （`rejected":["做法一：先深拷贝再合并"]`，理由"测试仍然失败，已放弃"，35 token）。
- 结论：**0.6B 不是不能用，是不能让它做"从原始记录里找结构"这件事** —— 这正是
  digest 该干的活。**给模型的输入质量比模型大小更决定成败**，这也是 [principles.md](principles.md) §4.4 的机制价值。

### 6.3 切段（episode 边界，确定性）

候选边界：①一条新 user 消息且上一段已有终局结论 ②任务终态事件
（测试全绿 / commit / turn 结束且无 pending）③积压事件数达阈值 ④时间间隔超过阈值。

**实现状态（2026-10-14）**：切段只有两条判据落地（`crates/context/src/digest.rs` 的
`segment()`）：新 user 消息且上一段已有 `SEGMENT_MIN_EVENTS = 4` 条事件，或距上一段
起点超过 `SEGMENT_GAP_MS = 10 min`。②④的"阈值进配置"**没有落地** ——
`[context_engine] episode_events` 虽被解析，但还没有被任何一段代码读取。
可重放这一条有测试保证（同输入必得同边界）。

### 6.4 上行触发

**实现状态（2026-10-14）**：

- **L3**：某模块的 L2 数达到 `DOMAIN_ROLLUP_EPISODES = 3`（**不是草案里的 8**，
  见 `crates/context/src/synthesis.rs`）时派生该模块的 L3；内容未变则不写库。
- **L4**：`Store::global_map()` 每次**从当前全部 L2 现算**（不落库、不做增量判脏）。
  这比"任一 L3 变化才重算"更简单，也总是反映此刻；代价是每次 `refresh()` 都要扫一遍
  L2 摘要——实测在 4 000 条量级仍是毫秒级（[rollout.md](rollout.md) §14 未决项 4）。
- "最小重算间隔 / 必须重算"两类条件与"脏集合按实体归模块"**尚未实现**：当前
  `digest_domains()` 遍历所有 domain 判断内容是否变化，没有显式脏集合。

### 6.5 走一遍（同一个 bug 的三次压缩）

假设一个 20 步的会话：用户报"配置层叠失效"→ 读 6 个文件 → 试方案 A → 测试失败 →
改方案 B → 测试通过 → commit。

| 层 | 产物（示意） | 谁生成 | 体积 |
|---|---|---|---|
| Raw | 60 条事件（含 6 次 `read_file` 的完整输出、2 次 `cargo test` 全文） | — | ≈28k token |
| L1 | 60 条 `atom`（`tool=read_file path=…`、`test:layer_merge FAILED/ok`、`commit:…`） | 确定性 | ≈9k token |
| L2 | `episode:配置层叠失效` — 结论：深拷贝导致的层叠丢失，方案 B 修复<br>`rejected[]`：方案 A（只改 merge 顺序）— 原因：测试仍失败<br>`status: done`，`citations: [[118,143]]` | digest 确定性<br>synthesis 模型 | ≈120 token |
| L3 | `domain:src/config` — 8 条 episode 的合流；本次新增"合并顺序不可依赖调用序"这条不变量 | 模型 | ≈400 token |
| L4 | 全局地图新增/更新一行：`src/config` — 分层解析已稳定；不变量 3 条；最近变更：深拷贝修复 | 模型 | 全局 ≈380 token |

三次压缩之后：前台从 28k + 全局地图，降到"工作集 + 380 token"。
而"方案 A 为什么被否决"仍然一步可达：`recall(query="层叠失效")` → L2 摘要（120 token，
已含 `rejected[]`）→ 需要时 `expand` → 事件 118–143 的原文摘录。

**这个例子同时说明为什么 digest 也必须存在**：如果模型不可用（R 模式），
L2 退化成"改了哪些文件、跑了什么、结果如何"的事实清单 —— 它给不出"为什么"，
但它仍然把 28k 压到几百 token，并且每一条都还能回到原文。R 模式不是残废模式，
是**没有理解、但检索完整**的模式。

---

## 9. 事件种类

后台工作与摘要变更都必须是**可见事件**，否则 ADR-0006 的"没有隐藏的注入"就破了。
拟新增四类（`crates/schema/protocol.toml` → `scripts/gen-schema.py` 生成）：

> **实现状态（2026-10-14）：这四类事件目前一个都没有。** `protocol.toml` 里没有
> `memory.*` 事件，引擎也从不写日志 —— 它只读会话日志、只写自己的 sidecar。
> 现状是：水位在 `meta` 表、摘要在 `summary` 表、worker 统计在进程内的
> `ContextExplain.memory_worker`，**都不是事件**。所以"摘要变更可审计、水位不是
> 暗状态"这条承诺**还没有兑现**。`materialize.rs` 已经
> 认得 `memory.summary.updated` / `memory.superseded` 两个 kind（遇到时按可检索事件
> 物化），但那只是为将来预留的读取路径，不是有人在写它们。

| 事件 | 字段（要点） | 为什么必须是事件 |
|---|---|---|
| `memory.l1.materialized` | `seq_from`, `seq_to`, `atoms`, `entities`, `tokens` | 让"哪些事件已被索引"可审计、可续跑；水位不是暗状态 |
| `memory.summary.updated` | `id`, `level`, `scope`, `producer`, `model`, `tokens`, `citations` | 摘要进入了前台上下文，它出现在日志里才算可解释 |
| `memory.superseded` | `id`, `by`, `reason` | 结论被取代是**历史事实**，不能静默覆盖（[principles.md](principles.md) §4.1） |
| `memory.worker.state` | `state`(idle/working/deferred), `pending`, `watermark`, `model_calls` | 后台花了多少、为什么停，是可查询的账 |

约定：这四类都 `ignorable = false`（核心事件）—— 一个读不懂摘要事件的读者，
不该假装自己理解了这次会话的上下文是怎么被组织的。摘要**正文**不进事件，
只进 `summary` 表：日志是真相，但真相是"摘要存在且指向这些区间"，不是摘要文本
（全文进日志会让日志体积翻倍，而它对重建没有增量价值）。
