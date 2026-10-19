# 非目标、阶段与实测依据

Language: zh

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0006](../decisions/0006-context-engine.md) · [ADR-0017](../decisions/0017-layered-context.md)
- **Code:** `crates/context/tests/faithfulness.rs`, `crates/context/tests/search_cost.rs`, `crates/context/src/lib.rs`
- **Verify:** `cargo test -p loong-context --test faithfulness -- --ignored --nocapture` · `./scripts/check.sh`

## 12. 非目标（明确不做）

- **不做跨会话的自动记忆共享**（ADR-0010 的立场）；跨会话复用需要显式导入。
- **不做向量检索作为主路径**。关键词+实体为主，语义检索是可选增强；
  理由是主路径必须能被 `grep` 式的确定行为解释。
- **不把摘要当训练数据 / 不写入控制面**。
- **不用主模型做归纳**（成本与"可选装"语义都不允许）。
- **不在本轮重写 `Ledger` 的预算/淘汰算法**，只加"原地替换 + 新分类"。

---

## 13. 分阶段实现（每阶段可独立验证、可停在任意一步）

| 阶段 | 状态 | 内容 | 完成判据 |
|---|---|---|---|
| **P1** 真相先可检索 | ✅ **已实现（2026-10-13）** | `crates/context`：L1 物化（确定性抽取 + 内容哈希去重）、实体与关键词索引、增量摄入水位、`rebuild`/`verify` | 按关键词/路径/模块找回任意历史事件；删库重建结果一致；2 字中文可命中（实测表见 [data-model.md](data-model.md) §5.3） |
| **P2** 派生摘要可读 | ✅ **已实现（2026-10-13）** | L2 的**确定性一半**：切段、digest（改动文件/命令/测试成败转变/状态）、`summary`/`citation`/`summary_entity`/`summary_inputs` 落库、`recall`+`expand` 只读路径 | 任意 L2 都能展开到原文；引用区间全部有效且落在自身区间内；派生血缘记录为「原始事件区间」（非另一条摘要）；12 个测试覆盖 |
| **P3** 后台常态 | ✅ **已实现（2026-10-13）** | 字节 tail（`tail.rs`）、增量 `sync`、`digest_pending` 补积压、`worker::step`/`run_activation`（前台让出 + 预算 + 统计）、截断自动重建 | 空闲看待机 27 µs；追加一个任务 8.4 ms 且不随日志增长；前台忙时 `Deferred` 且不读日志；**增量派生 == 全量重建**（有断言） |
| **P4** 前台变浅 | ✅ **已实现（2026-10-13）** | L3 域地图（确定性 rollup）+ L4 全局地图（硬上限 400 token）+ 内核 `Category::CognitiveMap`、`Ledger::replace`、`refresh_map`；**引擎开启时不再丢弃任何交换** | L4 是**独立 system 消息**、置于系统提示之后、只一条且原地刷新；引擎开启的长会话 `dropped_messages == 0` 且首轮请求仍在；引擎关闭时行为与改造前**完全一致**（有对照测试） |
| **P5** 语义层可选 | ✅ **已实现（2026-10-13）** | `Summarizer` trait、Ollama 原生客户端（`/api/chat`、`think=false`、JSON schema）、`Store::synthesize`（**纯增量**：合并而非覆盖）、**`recall`/`expand` 工具**（[recall.md](recall.md) §7.2）、`[context_engine]`/`[summarizer]` 的 `Profile` 解析（含 ADR-0008 权威：企业层可强制 `enabled=false`，用户层不能放宽；仓库层不得选 `summarizer.base_url`/`model`）、忠实度测试 | 未装模型 = R 模式行为（`NoSummarizer`）；装了 = 只补 `rejected`/`why`/`open_questions`/`conclusion`，**计算字段与引用一律不可被模型改写**（6 个测试）；**忠实度实测见 §14 未决项 2**：0.6B 编造 0、3B 编造 3；`recall` 有 6 个内核测试；`Profile` 侧 5 个测试；`ContextEngine` 测试（R 模式不碰模型、semantic 在后台并落库、模型不可达只降级那一段、未知 kind 失败关闭） |
| **P5.5** 空闲窗口与可见性 | ✅ **已实现（2026-10-14）** | 模型调用移出前台：独立 worker 线程 + 第二个 `Store` + `WindowBudget`（`window_calls`/`window_tokens`，narrow-only 取最小）+ 整轮 busy 且在每次调用前让出；`ContextExplain` 的 `memory_mode`/`memory_summarizer`/`memory_degraded`/`memory_layers`/`memory_worker`；TUI F2 与状态栏 | **前台 6 次步边界 17–19 ms，而同一模型调用在前台要 301 ms**（`#[ignore]` measurement）；一次空闲窗口实测（7B）：窗口 1 = 4 段 / 10.44 s（含模型加载），窗口 2 = 4 段 / 5.50 s，合计 15.94 s；`refresh_never_calls_the_model` 结构性门禁；两个写者同库实测；TUI "off" 与 "on 还没产出" 显示不同、降级有测试 |
| **P6**（待定） | 待定 | 内置推理：先做 candle spike（纯 Rust，决定是否需要 C++），再决定 `llama-cpp-2` 可选 feature 与 `agent-full` 产物 | 由 §14 未决项 1 拍板后再立项；**当前默认是 Ollama，P6 不是前置条件** |

每个阶段的测试要求：**同一输入必得同一输出**（确定性部分）、**引用可校验**、
**模式降级不损坏记忆**。

---

## 14. 待讨论（本轮要拍的）

1. **小模型后端 —— owner 已定：本机 Ollama（2026-10-13）**

   `qwen3:0.6b` 已在运行。本机实测环境：Ollama `0.40.3`，
   `qwen3:0.6b` = 751.63M 参数 / **Q4_K_M** / 522 MB / 40960 上下文 / capabilities
   `completion, tools, thinking`；另有 `qwen2.5:3b`（3.1B / Q4_K_M / 1.9 GB / 32768 /
   `completion, tools`，**不支持 thinking**）。

   **实测发现（三条都会改写实现，不只是配置）**：

   | 发现 | 实测 | 后果 |
   |---|---|---|
   | `/v1/chat/completions` **忽略 `think:false`** | 推理过程进 `reasoning` 字段并计入 `completion_tokens`；200 token 预算被推理链吃光，`content` 只剩"配置深拷贝导致层叠"半句 | 必须走**原生 `/api/chat`**（或自己发 `chat_template_kwargs`）；`providers::OpenAiCompat` 不能直接复用 |
   | **0.6B 做不了"从原始记录里找结构"** | 把事件清单直接丢给它，`rejected[]` 变成整段复述，`status` 乱填 | digest 必须先做（[pipeline.md](pipeline.md) §6.2）；模型的输入是**归纳好的事实句**，不是原始事件 |
   | 但 **0.6B 能做"单点语义判断"** | 输入改成事实句、只问"哪个做法被否决、为什么"，一次答对（35 token） | 两档模型：难字段用 3B，单点字段可用 0.6B |
   | `qwen2.5:3b` 也会错**算得出来的字段** | `conclusion`/`rejected` 基本正确，但 `status` 填成 `open`（实际 `done`） | **`status` 等字段一律由程序算**（[principles.md](principles.md) §4.4） |

   **性能实测（本机 CPU，结构化 JSON 输出）**：`qwen3:0.6b` decode ≈ **257 tok/s**、
   prefill ≈ **5 400 tok/s**、88 token 输出约 0.4 s；`qwen2.5:3b` decode ≈ **93 tok/s**、
   prefill ≈ **1 660 tok/s**。3B 一次完整归纳（约 400 token 输入 / 120 token 输出）
   约 **1–3 秒**，远低于原先保守估计的 10–30 秒。**延迟完全不是约束**。

   **决定（含 2026-10-14 的修正）**：
   - 传输层：`kind = "ollama"`（原生 `/api/chat`），`think = false`，`format` 传 JSON schema；
   - 模型：**默认 `qwen2.5:7b-instruct`**（`DEFAULT_SUMMARIZER_MODEL`）。本项最初定的是
     `qwen2.5:3b`、备选 `qwen3:0.6b`，但 §14 未决项 2 的忠实度重测把默认改成了 7B：
     7B `why` 7/7、3B 6/7、0.6B 3/7。**"两个档位"（`model_fast`）没有实现** ——
     实测结论是"能算的字段由程序算、模型只答 `why`/`open_questions`/`conclusion`"，
     不需要按字段分流两个模型；配置里的 `model_fast` 会被拒绝（[config.md](config.md) §11）。
   - 不引入任何新的 Rust 依赖（`loong-http` 已够用），二进制零膨胀；
   - 用户环境需要 `ollama pull` 一个模型 —— 这是"可选装"的兑现方式，文档写明
     "未安装 = R 模式，一切仍可用"。

   **保留的备选**（不在本轮）：
   - `llama-cpp-2` 作为 `local-llm` 可选 feature 内置推理：需 cmake + C/C++（仅 CI 付一次），
     无构建期联网（vendored），预期 +15–40 MB（未实测）；出 `agent` / `agent-full` 两个产物；
   - `candle` 纯 Rust 路线先做 spike：其 API 表面对 `quantized_qwen2/qwen3/llama/gemma3`
     已就绪，但**未验证真能跑通生成**，且没有 chat template（要自己用
     `tokenizers::apply_chat_template` 接 GGUF 元数据）。spike 通过就不需要 C++ 工具链。
   - 自带推理时的模型候选（Apache-2.0 且中文强）：**Qwen3-0.6B**（Q4_K_M 397 MB /
     Q8_0 639 MB）、**MiniCPM5-1B**（688 MB，131k 上下文，candle 不支持）、
     Qwen2.5-1.5B（1.1 GB）。⚠️ Gemma / Llama / LFM 系许可证非 OSI，企业内工具建议排除。
     Ollama 路线下这些都不是前置条件，**P6 可以无限期推后**。

2. **采纳门槛 = 忠实度测试 —— 已实现并已实测，抓到了选型的关键事实**

   门槛本身已落地（`crates/context/tests/faithfulness.rs`）：10 份真实中英 digest +
   ground truth（被否决项 / 未决问题 / 状态 / 是否该保持沉默），两个测试分开 ——
   打分器本身是**gate**（无模型也跑，用一个已知忠实与一个已知编造的回答验证仪器），
   真机测量是 `#[ignore]` 的**measurement**。

   打分器演进过一次：最初只查"禁用短语表"，于是 `qwen2.5:3b` 在"本轮没有否决任何方案"
   的案例上编造出「使用缓存替代」时**没被发现**。改成"每一个被命名的否决都必须能被
   ground truth 解释"后才发现，并且同时避免了反向误伤（同一个放弃项换一种说法算正确 ——
   **措辞不是忠实度**）。

   **2026-10-14 重测（门槛改成只测模型真正负责的 `why` 之后）**：

   | 模型 | `why` 命中 | 解释错了对象 | 说不出原因 | 编造（harness 侧） |
   |---|---|---|---|---|
   | **`qwen2.5:7b-instruct`（4.7 GB）** | **7/7** | **0** | **0** | 0 |
   | `qwen2.5:3b`（1.9 GB） | 6/7 | 1 | 0 | 0 |
   | `qwen3:0.6b`（0.5 GB） | 3/7 | 4 | 3 | 0 |

   三个模型都**不再编造** —— 因为 `rejected` 已经改成程序计算，模型无从编造。
   差别落在 `why`：7B 每次都解释对了对象（"按层号排序后合并"），
   3B 有一次把**被放弃**的做法当成原因（"通过改变测试的执行顺序来消除依赖"——
   而那正是被否决的那次尝试），0.6B 有 4 次解释错对象、3 次干脆不说。

   7B 的 `why` 全部正确，例如「通过按层号排序后合并，确保了配置项在合并时遵循了正确的
   优先级顺序」；`abandoned` 由 harness 算出且全部正确。**默认模型已改为
   `qwen2.5:7b-instruct`**（`DEFAULT_SUMMARIZER_MODEL`）。

   更早一轮的实测（2026-10-13，`rejected` 仍由模型回答时）——保留作为教训，
   因为其中"3B 编造"有一部分是我的输入没有记录尝试序列逼出来的：

   | 模型 | 漏掉 | **编造** | 说不出原因 | 判定 |
   |---|---|---|---|---|
   | `qwen3:0.6b` | 7/10 | **0** | 2/10 | 忠实但很薄 |
   | `qwen2.5:3b` | 7/10 | **3/10** | 0/10 | **不可用作记忆** |

   3B 编造的正是最危险的形状：在"本轮没有放弃任何方案"的案例上凭空说
   「使用缓存替代」「请求权限检查」「使用了其他工具修改 README」。更值得注意的
   一条规律：它倾向于把**用户请求本身**填进 `rejected`。

   **当时的结论（2026-10-13，已被上面的 2026-10-14 重测取代）**：0.6B 更安全、3B 更危险；
   默认选 `qwen3:0.6b`。**现在默认是 `qwen2.5:7b-instruct`** —— 上面那张表（门槛改成只测
   `why`、`rejected` 已由程序算之后）才是当前依据。保留这一段是因为它记录了一个真实教训：
   当时"3B 编造"有一部分是**输入没有记录尝试序列**逼出来的，换模型之前先修输入。

   重跑方式：`cargo test -p loong-context --test faithfulness -- --ignored --nocapture`
   （可用 `LOONG_FAITHFULNESS_MODELS` / `LOONG_OLLAMA_URL` 换模型与端点）。
   换模型、换提示、换 schema 之后**必须重跑**：这份表格是选型的唯一依据。

3. **CJK 关键词索引方案（已实验；第一轮结论已被自己的实测推翻一次，以下是修正版）**
   - 可依赖性：仓库现有的 `rusqlite 0.40 + features=["bundled"]` 在 `libsqlite3-sys 0.38.2`
     的 bundled 构建里**无条件带上 `-DSQLITE_ENABLE_FTS5`**（已核对 vendor 源码），
     无需新增依赖；`bundled-full` 只多给 rusqlite 的 API，与 FTS5 无关。
   - `unicode61` 对中文**完全无效**（`深拷贝`/`工具输出` 命中 0），
     `tokenize='trigram'` 对中文短语与中英混排全部正确命中，2 万行 3 字查询 ~0.5 ms。
   - **硬限制**：trigram 要求查询 ≥3 字符；2 字中文词（`修复`/`方案`/`拷贝`）命中为 0。
     更危险的是 FTS5 的隐式 AND：多词查询里只要有一个 <3 字的词，**整条查询静默返回 0**。
     所以关键词必须**逐个路由**：≥3 字走 MATCH，<3 字走 `LIKE`。
   - **`detail=none` 不能省空间**（我第一版写错了，已实测推翻）：
     `detail=none` + trigram 只支持**恰好 3 字**的查询，4 字以上直接报
     `fts5: phrase queries are not supported (detail!=full)`。故 `detail` 保持默认 `full`。
   - **contentless 表不能配 LIKE 兜底**：`content=''` 时除 rowid 外所有列读出为 NULL，
     `LIKE '%修复%'` 返回 0。**必须用外部内容表**（`content='summary', content_rowid='rowid'`），
     此时 MATCH 与 LIKE 都工作。代价是正文在 `summary` 表里存一份（摘要很小，可接受）。
   - **磁盘放大是真实的**（20k 混合中英事件，源 3.17 MB，`optimize`+`VACUUM` 后）：

     | 变体 | 仅索引 | 库总计 |
     |---|---|---|
     | trigram `detail=full` | **208% of source** | 318% |
     | trigram `detail=none` | 77% | 186%（但 MATCH 残废） |
     | char-unigram（汉字间插空格 + unicode61） | 75% | 184% |
     | 词典分词 + unicode61 | 23% | 133% |

     这正是 [data-model.md](data-model.md) §5.3 把全文索引**只建在摘要上**、原文走 `LIKE` 的原因。
   - 排序：bm25 的 `|D|` 是 token 数，中文（≈1 token/字）与英文（≈1 token/词）的
     长度惩罚不对称，跨语言排序会偏。对 grep 式检索**先按 `seq`/时间倒序，bm25 只作 tiebreak**。
     同理 `detail≠full` 时不要用 bm25（词频为 1）。
   - 查询构造：每个关键词都要用引号包成 FTS5 字符串字面量（转义内部 `"`），
     否则路径与散文里的 `AND/OR/NOT/^/:/*/NEAR` 会**静默改变语义**。
   - **~~结论（待 owner 确认）：主路径 = 摘要上 trigram FTS5 + 原文 LIKE 兜底 + 实体精确过滤~~**
     **这条已被下面的未决项 4 与实现推翻**：最终**不建任何 FTS5 表**，摘要与原文都走
     `LIKE` + 实体/工具/kind 精确过滤（4 000 条摘要扫描 3.8–5.6 ms，够快）。
     不引入中文分词字典这一条保留（无字典体积、无分词分歧、对代码标识符与 CJK 一视同仁）；
     "若实测 2 字召回或性能不足优先加 char-unigram 派生列"仍是后备方案，目前没有加。
   - 工程注意（便宜但容易忘）：WAL 下**主库文件大小在 checkpoint 前没有意义**
     （实测 7.44 MB → checkpoint 后 11.00 MB）；批量重建用
     `journal_mode=OFF`+`synchronous=OFF`，服务期切回 WAL；`optimize` 只在重建时跑；
     每次建库都 `PRAGMA wal_checkpoint(TRUNCATE)` 后再比较体积。

4. **摘要是否需要全文索引 —— 已实测定案：不要**
   原设计（本文件早期版本与 ADR-0017 未定项 2 的延伸）打算用
   `summary_fts`（trigram + 外部内容表）。实测量级：4 000 条摘要下 LIKE 扫描
   **3.8–5.6 ms**、FTS5 MATCH 约 **0.12 ms**；两者都在"人感知不到"的量级，
   而外部内容 FTS5 带来一整类正确性问题，实现期间**每一个都真的踩到了**：
   ① 按 rowid 查询会从正文表回答，所以"这条索引过没有"无法用 `COUNT(*)` 判断，
   我的守卫因此失效；② 对**从未索引过**的 rowid 执行 delete 会报
   `database disk image is malformed`（把正文行拿去反索引），看着像数据库损坏；
   ③ 每次写入都要手工保持同步。结论：**摘要走 LIKE 扫描，不建全文索引**，
   理由与数字都留在 `crates/context/tests/search_cost.rs` 的
   `summary_recall_cost` 里 —— 如果某个会话的摘要数量涨到扫描真的变慢，
   那份测量就是推翻这个决定的证据。

5. **是否需要"摘要版本化"**：当 L2 被重新生成（模型升级/证据更新）时，旧版本保留
   还是只保留 `superseded_by` 链？倾向前者（可审计），但要算存储成本。

6. **L3 模块映射的粒度**：`src/kernel` 还是 `src/kernel/turn.rs`？太细则 L3 数量爆炸，
   太粗则模块地图没信息量。**实现已按"前 2 段"落地**（`synthesis::domain_of`，与
   `materialize` 的 `module` 实体同一规则）；草案里"显式 `[[context_engine.domains]]`
   覆盖"**没有实现**，配置里也没有这个键（[config.md](config.md) §11）。粒度要改是"一个函数 + 一次重派生"，
   见 `synthesis.rs` 顶部注释。

7. ~~前台是否注入 L3~~ **P4 已按"L4 常驻 + L3/L2 按需"落地**：L4 硬上限 400 token
   常驻，L3 与 L2 走 `recall`/`expand`。理由是 L4 的边际成本是常数，而 L3 的数量
   随模块数增长 —— 常驻 L3 会让"最浅表上下文总是最精简"这条一开始就不成立。
   若将来要常驻当前任务的 L3，应是**注入一个、随任务切换**，而不是全部注入。

8. **跨会话复用**（T-10）是否纳入路线图：同项目的历史会话摘要天然是团队知识。
   ADR-0010 的立场是知识走 git，不进共享会话 —— 这个方向可以是一条独立 ADR。
