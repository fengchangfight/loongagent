# Limits and landed pieces — 已落地与仍缺

Language: zh

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0006](../decisions/0006-context-engine.md) · [ADR-0017](../decisions/0017-layered-context.md)
- **Code:** `crates/context/src/store.rs`, `crates/context/src/worker.rs`, `crates/kernel/src/context_engine.rs`
- **Verify:** `cargo test -p loong-context -p loong-kernel` · `./scripts/check.sh`

## 11.1 缺口清单与已落地（2026-10-14 更新）

诚实清单，避免"文档说完成了"被读成"都能用了"：

| 缺口 | 状态 | 备注 |
|---|---|---|
| **模型调用还没"空闲窗口批量"** | **✅ 已实现（2026-10-14）** | 模型调用已从 `ContextEngine::refresh` 移出，交给独立后台线程按空闲窗口批量富化。落法与实测见下 |
| ~~CLI `loong context status/verify/rebuild`~~ | **已实现（2026-10-14）** | `status` 报水位/实体/各层条数/producer 分布，并在不健康时提示；`verify` 逐条检查引用区间；`rebuild` 丢弃并从日志重建。**读命令不自动建库** |
| **TUI 没有记忆状态显示** | **✅ 已实现（2026-10-14）** | F2 面板显示引擎开/关、record/semantic、summarizer、L1–L4 条数与 token、worker 统计、降级原因；状态栏在降级时显示 `mem ⚠ degraded`。**"关着"与"刚开还没产出"必须显示不同**，靠的是如实字段而不是"账本里有没有 cognitive_map" |
| **Web 面完全没动** | 仍缺 | 本会话明确不做；`ContextExplain` 的新字段已进协议，Web 侧暂不渲染 |
| ~~默认模型~~ | **已改为 `qwen2.5:7b-instruct`（2026-10-14）** | 门槛实测：`why` 7/7、解释错对象 0、说不出原因 0 |
| **`recall` 的 `expand` 只用摘要 id** | 仍缺 | 模型要两步（先 recall 拿 id，再 expand）。可用，但多一轮往返 |
| **一次性 headless 运行没有空闲窗口** | 已知取舍 | 前台"整轮 busy"信号最保守：一轮里后台绝不打模型，代价是 `loong run "…"` 这种一问即退的运行不会触发富化（digest 仍然完整，库仍可 `rebuild`）。要富化就多轮交互，或将来把窗口细到"工具等待" |
| **`memory.*` 事件一个都没落地** | 仍缺（**2026-10-14 审计发现**） | [pipeline.md](pipeline.md) §9 的四类事件（`memory.l1.materialized` / `memory.summary.updated` / `memory.superseded` / `memory.worker.state`）在 `protocol.toml` 里不存在，引擎也从不写日志。摘要变更因此**不能从日志审计**，水位与 worker 统计只在 `meta`/进程内。ADR-0017 的 Consequences 曾把它写成已新增，现已在那边标注"尚未落地" |
| **`superseded_by` 从未被写过** | 仍缺（**2026-10-14 审计发现**） | 列存在、读取路径存在，但 `replace_overlapping` 对过期摘要执行的是 `DELETE`，不是"标 `superseded_by` 保留"。ADR-0017 [pipeline.md](pipeline.md) §6 的"旧摘要标 superseded_by，不静默覆盖历史"**没有兑现**；当前语义是"重叠即重派生" |
| **`l4_max_tokens` / `episode_events` 解析了但没人读** | 仍缺 | 两个键进配置后会合并进 `ContextEngineSettings`，但 L4 上限仍是常量 `L4_MAX_TOKENS = 400`、切段仍是 `SEGMENT_MIN_EVENTS = 4`。一个"接受但无效"的键比一个报错的键更危险，所以单列（[recall.md](recall.md) §7.1、[pipeline.md](pipeline.md) §6.3） |
| **一批草案键根本没接线** | 仍缺 | `store` / `episode_gap_secs` / `domain_rollup_episodes` / `recall_limit` / `recall_max_tokens` / `model_fast` / `think` / `timeout_ms` / `on_error` —— `deny_unknown_fields` 下写入即报错（[config.md](config.md) §11） |
| **L3 脏集合 / 上行最小间隔** | 仍缺 | [pipeline.md](pipeline.md) §6.4 的"按实体标脏 + 只重算脏模块 + 最小重算间隔"没有实现；`digest_domains()` 每轮遍历所有 domain 比较内容，L4 每轮从全部 L2 现算。规模上目前是毫秒级，但这是"新内容只处理一次"承诺里唯一没有做到 O(增量) 的部分 |
| **CLI `context build` / `context explain` / TUI `/context`** | 仍缺 | [config.md](config.md) §11 里列过；实际只有 `status`/`verify`/`rebuild` 与 F2 面板 |

### 这两件怎么落的（2026-10-14 已实现）

**空闲窗口批量归纳**。`refresh()` 只做**确定性**派生（`sync` / `digest_pending` /
`digest_domains`）并唤醒后台；模型调用在一条独立线程上，按
`window_calls` / `window_tokens` 的硬预算开一个**空闲窗口**，窗口之间休眠，
`Foreground::is_busy()` 为真时让出。几个决定：

- **第二个 `Store` 连同一个 sidecar**，不是 `Arc<Mutex<Store>>`。共享一把锁会让前台
  等在后台的**模型调用**上，正是 [principles.md](principles.md) §4.6 要避免的失败；两个连接只在 SQLite 的短写事务上
  串行（WAL + `busy_timeout`），模型请求期间没有事务在手。实测：`Store::open` 设置的
  5 s `busy_timeout` 能让输掉竞争的写者等赢家提交后继续；`rusqlite` 自己恰好也有 5 s
  默认值，但我们把它写出来 —— 保证是这套设计的，不是驱动的默认值
  （`crates/context/tests/two_writers.rs` 里有对照组：超时为 0 时同样的写在 300 ms
  的锁下**立即**报 `database is locked`）。
- **线程归属：引擎（`crates/kernel/src/context_engine.rs`），不是 server 也不是 CLI。**
  两者都通过同一个工厂建 kernel（`crates/cli/src/server.rs`），`session.new` 换掉的是
  整个 kernel，一次性 CLI 运行根本没有会话生命周期对象 —— 把生命周期放在任一面，
  另一面就会忘了启动或停止。引擎是唯一同时知道自己那份日志、那个库和那个前台信号的
  组件。`Drop` 有界等待 250 ms，等不到就放线程走：换会话不该等一次飞行中的模型调用。
- **前台信号：整轮算 busy，按调用让出。** 内核在一轮开始时置 busy、结束时清并唤醒。worker
  进窗口前查一次；窗口内部也在**每次模型调用前**再查（`Foreground` 由内核的 busy 标志
  驱动），所以一轮进行时后台不会开新窗口，一个已经开着的窗口也会在下一次调用前停下。
  前台**不等它**：两个连接、没有共享锁，飞行中的请求跑在 Ollama 自己的进程里，SQLite
  只在短写事务上串行。设计里更细的窗口（工具调用等待时也允许富化）没有做 —— "整轮 busy"
  是更保守的读法，也是"一轮不可能被拖慢"最容易验证的形式。
- **失败是终局的（对同一个模型）。** 一次失败写进 `producer`
  （`digest+model:<name> (failed: …)`），该段不再被问第二次；否则一个连不上的端点会让
  worker 永远到不了 [principles.md](principles.md) §4.5 要求的稳态。换一个模型名是另一个问题，会重问。失败也**计入**
  窗口的调用预算，否则一个坏端点在单个窗口里会横扫整个会话。
- **候选行用 SQL 选，不再用 `OFFSET` 翻页。** 旧实现每次窗口都从第 0 条开始 `OFFSET`
  跳过已富化的段，一条会话跑到几千段后，一个窗口的成本变成 `O(历史)`；新实现直接按
  `producer` 过滤出待富化的少量行。
- **模型答案只在"底稿没变"时落库。** worker 在窗口**之间**让出，不在窗口**内部**，
  所以一次飞行中的请求可能跨过一个新 turn：前台重算了这段、模型才回答。写入因此是一条
  带 `WHERE content = <读到的底稿>` 的条件 `UPDATE`（检查与写入原子），底稿变了就丢弃
  这次答案、留给下个窗口 —— 否则 `INSERT OR REPLACE` 会把前台刚写的新 digest 覆盖回旧的、
  并复活一个过期的段边界（`crates/context/tests/semantic.rs` 的
  `a_summary_replaced_while_the_model_answers_is_not_overwritten` 钉住这一点，
  去掉 `WHERE` 该测试即失败）。

实测（本机 Ollama，`qwen2.5:7b-instruct`，window = 4 calls / 6000 tokens，
`crates/context/tests/window_cost.rs`，8 个已闭合任务 / 40 atoms / 8 episodes）：

| 窗口 | 墙钟 | 调用 | 富化 | 输入 tok | 输出 tok | 剩余 |
|---|---|---|---|---|---|---|
| 1 | 10.44 s | 4 | 4 | 326 | 97 | 4 |
| 2 | 5.50 s | 4 | 4 | 326 | 113 | 0 |

合计 8 次调用、8 段富化、15.94 s；窗口 1 含 7B 模型加载，稳态约 1.4 s/次。
同日重跑一次为 10.50 s / 5.62 s、合计 16.12 s —— 模型延迟本身有波动，这个表说明的是
量级，不是常数。确定性部分（`rebuild` + `digest`，即前台在关键路径上做的）是
**7–10 ms**。

前台不被拖慢的直接实测（`crates/kernel/src/context_engine.rs` 的 `#[ignore]`
measurement）：24 个任务下 6 次步边界（确定性派生）共 **17.2–18.5 ms**；同一个模型调用若在
前台，单次就要 **301 ms**。验收门禁则是结构性的
（`refresh_never_calls_the_model`：步边界只派生、不调模型），因为"没有模型调用在
关键路径上"不依赖机器速度就成立。

**TUI 的记忆状态**。协议侧新增 `ContextExplain.memory_mode`（引擎关时为空，
否则 `record` / `semantic`）、`memory_summarizer`、`memory_degraded`、
`memory_layers[]`（L1–L4 条数与 token）、`memory_worker`（state/windows/model_calls/
enriched/failed/pending/deferrals/last_error）。要点：

- **`memory_mode` 是唯一的"开/关"判据**，不是"账本里有没有 `cognitive_map` 条目"：
  语义引擎开着但头几个 turn 还没产出 L4 时，账本里同样没有该条目，用条目判断会把
  "刚开还没东西"读成"关着"。面板对这两种状态显示不同文字（`memory off` vs
  `semantic … · no L4 map yet`），有测试（`an_engine_that_is_off_is_not_shown_as_a_memory_that_has_produced_nothing`）。
- **降级必须说出来**：`memory_degraded` 合并了引擎级失败（库打不开/刷新失败）与
  "N 段退回 digest"；`memory_worker.last_error` 给出最后一次模型错误，状态栏在降级时
  常驻 `mem ⚠ degraded`。没有静默降级。
- **`context_explain(&self)` 是只读查询**，所以降级状态与 worker 计数放在
  `Arc` 的共享状态里（原子 + 短锁），由内核 `&self` 读；查询本身不写任何事件。
