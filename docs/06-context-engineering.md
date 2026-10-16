# 06 · Context engineering — 分层可逆记忆

本文是设计文档，不是决策记录。一旦方向定下，其中的决策点落入
ADR-0017（`docs/decisions/0017-layered-context.md`）。讨论中仍未定的问题见 §14。

## 0. 一句话

把上下文管理从**有损压缩**改成**可逆的信息组织**：原文永不离开事件日志，前台只携带
少量"认知摘要"，需要时用关键词索引一层层定位回原始证据。

对照两种模型：

| | 传统（Claude Code 式） | 本文 |
|---|---|---|
| 触发 | 越过 threshold 才偶发压缩 | 常驻后台、增量、稳态即休眠 |
| 动作 | 总结旧内容 → **替换**原文 | 提取/归纳/建索引 → 原文**留底** |
| 前台 | 压缩后的残骸 | 极薄的 L4/L3 摘要 |
| 恢复 | 原则上不可逆（只能靠日志考古） | 一步可达：`recall` → 引用区间 → 原文 |
| 成本 | 随历史长度**线性**增长（每次重读全文） | 每条事件**摊还常数**，模型只在"归纳"处被调用 |

**必须现在说清的一个限定。** "不存在有损压缩"只在**检索**意义上成立：原始记录永远可
逐字取回。但在**理解**意义上一定有损 —— L4 的"这个项目现在处于什么阶段"本身就是模型
的加工品，不是事实。所以本设计的纪律是：**摘要永远是索引，不是事实；一切声明必须落到
引用区间**。§7.3（系统提示纪律）、§7.2（`expand` 强制带原文）与 §8（风险表）把这个纪律
做成机制，而不是靠约定。

---

## 1. 现状（改造的起点）

| 组件 | 位置 | 今天做什么 |
|---|---|---|
| 事件日志 | `crates/eventlog` | 每会话一个 append-only JSONL，`seq/id/parent_id/kind/time_ms/data`；**唯一真相**（ADR-0003） |
| 派生索引 | `crates/index` | SQLite `events(seq,id,parent_id,kind,ignorable,time_ms,data)`；可丢弃重算 |
| 上下文账本 | `crates/kernel/src/ledger.rs` | 每个进入模型的块是一条带 category/source/tokens/lifetime/pinned 的条目；**通往模型的唯一写路径**（ADR-0006） |
| 压缩 | `crates/kernel/src/turn.rs:1884` | 越 `input_ceiling()`（预算 × `compact_at_percent`，或 模型窗口 − reserve）时：①清旧工具输出 ②丢弃最老的整轮对话 |
| 可见性 | `ContextExplain` | 分类 token 明细、条目来源、压缩次数 |

现状的问题正是引言里那句：**替换原文**。`drop_oldest_exchange()` 把消息从
`messages` 里 `drain` 掉，`ledger.forget()` 抹掉条目 —— 前台从此只有残骸，
要回到"当时为什么这么设计"只能靠人翻 JSONL。

---

## 2. 目标结构

```
前台（发给模型的请求）
  ├─ system prompt / tools / rules / skills            [不变]
  ├─ L4  全局认知地图    Global Map    几百 token，常驻
  ├─ L3  主题/模块地图   Domain Map    按需注入（当前任务涉及的模块）
  └─ 当前会话的近期原文（工作集）                        [受预算约束]
            │
            │  recall(query, layer, entity, limit)      ← 模型唯一的检索入口
            ▼
派生记忆（SQLite，可丢弃重算）
  ├─ L2  事件/任务摘要   Episode Summary   带 status + 引用区间
  ├─ L1  原子记录        Atomic Record     确定性提取，近原文，带稳定 ID
  └─ 关键词/实体索引     Keyword & Entity Index
            │
            │  citation: [seq_a, seq_b]
            ▼
原始记录仓库  Raw Archive  = JSONL 事件日志（真相，只追加，永不删改）
```

### 2.1 与用户提出的五层结构的对应

用户给的划分里还有第五项"原始记录仓库 · Raw Archive"。本文不把它当作第五层摘要，
而是当作**底座**：L1 是它在派生库里的**物化视图**（为了建立索引所需的稳定 ID、实体、区间），
L2–L4 是它的**递归归纳**。这样 L4/L3/L2/L1 是四层，底座独立。

### 2.2 两层语义必须在文档和 UI 里分开

| | 检索层（L1 + 索引 + Raw） | 理解层（L2/L3/L4） |
|---|---|---|
| 内容 | 原文、原文的物化视图、关键词/实体 | 归纳出来的结论、状态、原因、被否决的方案 |
| 正确性 | 与原文一致（可校验） | **可能错**（模型加工） |
| 失效模式 | 索引坏了 → 重建 | 结论过期/漂移 → 需取代与再生成 |
| 缺席的影响 | 检索变慢 | 模型"忘事"（只能看到浅表上下文） |
| 无模型时 | 完整可用 | 降级为确定性 digest |

**产品后果**：理解层是"可选装"的那部分（§11）；检索层**永远开启**，因为它是
"不丢原文"的兑现。用户截图里的判断分支（是否语义归纳）＝ 这里的理解层开关。

---

## 3. 两种运行模式

### R 模式 · Record-only（默认回滚路径，零依赖）

- L1 物化 + 关键词/实体索引 + 确定性 digest（§6.2）全部工作，**不调用任何模型**。
- 前台携带"事实清单式"的 L4：最近任务列表、当前模块、状态、引用区间。
- 现有压缩**保留**：越过 ceiling 时清工具输出、丢最老交换（与今天一致）。

### S 模式 · Semantic（可选装，开启后显著增强）

- 在 R 模式之上，L2 的"归纳部分"、L3、L4 由小模型生成。
- 开启后**不再丢弃消息**（rollback 保留）：只清工具输出。
- 前台携带真正的 L4 认知地图，并暴露 `recall` 的 L2/L3 检索。

两种模式共用同一套存储、同一套引用语义、同一套 API；模式只改变**谁生成理解层**。

---

## 4. 原则（把引言里的四条钉成机制）

### 4.1 原文是唯一真相，摘要只是引用

- JSONL 是真相；SQLite（L1–L4 + 索引）是派生、可丢弃、可重建。
- 每条摘要**必须**携带 `citations: [(seq_a, seq_b)]`，否则不予物化。
- 索引丢失 → 重建；摘要丢失 → 重算（可从子层重算，代价可控，见 §4.3）。
- **不变量**：`loong context verify` 能检查每条摘要的引用区间在日志里存在、且区间未被
  改写（日志只追加，所以只可能"区间不存在"→ 越界=损坏）。

### 4.2 上行收敛，不是回看全文

每层的输入**只是它的直接子层**：

```
L4 ← (全部 L3 摘要) + 项目级不变量
L3 ← (该模块的全部 L2 摘要) + 模块实体清单
L2 ← (一段 L1 区间) + 该区间涉及的实体
L1 ← (新事件)                       确定性，无模型
```

这条规则决定了成本与质量**都不随历史长度爆炸**：L4 的每次重算读的是"模块数×常量"，
而不是十万 token 的对话。若 L4 直接读原文，历史一长就会既贵又坏（§8 风险表第 1 行）。

### 4.2.1 绝不"在上一版摘要上继续总结"（漂移防线）

这是上一条最容易被误读、也最危险的地方。**增量不等于链式**：

| 允许 | 禁止 |
|---|---|
| 只读**新增**的 L1 区间做新 L2 | 把旧的 L2 正文喂给模型说"请更新一下" |
| 重算 L3 时读**当前全部 L2 摘要**（每个都是子层自己从 L1 派生的） | 把上一版 L3 正文当作 L3 的输入 |
| 重算 L4 时读**当前全部 L3 摘要** | 把上一版 L4 正文当作 L4 的输入 |

也就是说：**摘要永远从它当前的子层重新派生，而不是从上一版摘要演化。**
未变的子层摘要**已经在库里**（不需要重读原文），所以"重派生"在成本上等价于
"只处理新增量"，但**误差不累积** —— L4 的每一个事实都有一个只经过一次归纳的
L2 祖先。若允许 L3 读旧 L3，四层就是四次迭代，误差按层数放大，且无法归因。

实现上这是 `summary` 的一条硬规则：生成任意一条摘要时，其输入集合是
「子层当前摘要 ∪ 本次新增 L1」，**旧版本自身不在输入集合中**，且这一条要在
代码里可检查（`producer` + 输入摘要 id 列表存档），而不是靠提示词约束。

### 4.3 新内容只处理一次

- 高水位：`watermark(kind)` 记录已处理到的 `seq`。新增事件只从水位之后读取。
  读取本身按**字节偏移**（tail）：一次查看只读新增的字节，空闲时只做一次 `stat`
  （实测 **27 µs**）。全量重读只发生在重建与恢复路径。
- 脏集合：新事件按实体归入受影响的 L3 模块，标脏；只有脏模块的 L3 与 L4 需要重算。
- 摊还：每条事件被处理**一次**；L2 被 L3 读一次；L3 被 L4 读一次。
  → 单条事件成本 = O(1)（确定性）+ O(一次归纳)（模型），与历史深度无关。

### 4.4 先确定性、后模型

| 交给程序（确定性、可测试、零成本） | 交给模型（语义、判断） |
|---|---|
| 工具调用类型、文件路径、命令、错误码、测试名 | 多条事件归纳出一条任务结论 |
| git diff、改动文件、测试结果 | 判断新结论是否**改变**了既有认识 |
| **任务状态**（`done`/`open`/`blocked`：由测试是否通过、有没有 commit、有没有 pending 批准**算出来**） | 区分事实 / 推断 / 计划 / 已验证 |
| 稳定 ID、去重（内容哈希） | 判断是否值得**上行**到更高层 |
| 按路径/符号/任务 ID 建立关联 | 自然语言归纳（"为什么这么设计"、"哪个方案被否决了"） |
| 关键词与实体索引、语义检索、**结论里的每个引用的存在性** | — |

**这条规则比它看起来更硬：凡是能算出来的字段，一律不许问模型。**
本机实测支持这一点：`qwen2.5:3b` 归纳"方案 A 被否决、方案 B 通过、已 commit"时，
`conclusion` 与 `rejected` 都基本正确，却把 `status` 填成了 `open`（实际是 `done`）——
状态是**算得出来的**，让模型猜就一定会错，而且错得很难发现。
所以摘要的字段分两类，`producer` 逐字段记录：

| 字段 | 谁填 | 依据 |
|---|---|---|
| `status` | **程序** | 测试结果 / commit / pending 批准 / 未决问题是否清空 |
| `seq_from`/`seq_to`、`citations` | **程序** | 子层区间与新增事件区间 |
| `touched_files[]`、`commands[]`、`tests[]` | **程序** | L1 实体与事件种类 |
| `rejected[]`、`open_questions[]`、`why[]`、`conclusion` | 模型（不可用时留空并标 `degraded`） | 只有这些真的需要理解 |

**副产品**：R 模式下整个系统仍然完整 —— 因为"记账与组织"从来不需要模型。
这也让 S 模式有明确的降级语义：模型不可用 = 退回 R，而不是记忆损坏。

### 4.5 稳态检测，而非持续空转

后台工作的判据（全部为真才休眠）：

```
cursor.offset == 文件长度  ∧  digested_through >= 已闭合段末尾  ∧  前台不忙  ∧  预算未耗尽
```

实现上分两类"没事可做"：**读**没事可做（偏移已在文件末尾）与**派生**没事可做
（`digested_through` 水位已覆盖所有已闭合段，末段派生结果与库中一致 → 不写库）。
后者必须显式判断，否则"末段永远要重算"会让稳态变成空转 —— 这是实现时真的踩到的。

唤醒源：①一轮交互结束 ②进入工具等待 ③队列积压达阈值 ④显式请求（`/context build`）。

**实现状态（2026-10-14）**：目前只有 ①（`set_foreground_busy(false)` 唤醒）和
"新内容派生完 `refresh()` 主动 `wake()`"落地；后台另有一个 `idle_sleep`（默认 250 ms）
的定时查看。②③④**尚未实现** —— 整轮算 busy 意味着工具等待期间不会富化，
队列阈值不触发唤醒，`/context build` 这条命令也不存在（见 §11.1 缺口表）。
稳态判据本身（读没事可做 / 派生没事可做）已按上面的分析实现并实测。

### 4.6 不与前台争抢

- 单飞行：任意时刻最多一个摘要请求在飞。（已实现：单线程 worker + 窗口内逐次调用。）
- 预算：每个空闲窗口最多 N 次归纳调用 / M token；用尽即休眠，下个窗口继续。
  （已实现：`SummarizerChoice.window_calls` / `window_tokens`，配置侧 narrow-only 取最小。）
- 前台一有新 turn，worker 立即让出（可中断的批次边界）。（已实现：整轮 busy，
  窗口内每次调用前再查一次。）
- 线程优先级降低，可选 `nice`；`max_threads = 1` 默认。**单线程已实现；
  `nice` 未做** —— 内核没有引入 libc 依赖，这一条留作可选增强。
- 绝不为了"实时"而阻塞一次工具调用或一次模型请求。
- **资源争抢取决于后端形态**：若摘要走本机 HTTP 端点（Ollama 等），模型在自己的
  进程里跑，harness 只是发一个请求 —— 零进程内争抢，这是 (a) 方案被低估的优点；
  若捆绑进程内推理，则必须放进**独立进程**（见 §14 未决项 1），否则会和主模型
  循环抢 CPU。这条差别应当直接写进配置文档，因为它决定用户是否感觉得到变慢。

---

## 5. 数据模型

### 5.1 位置

`<session>.context.db`，与 `<session>.jsonl` 同目录（沿用 `index` 的位置约定）。
删掉它 = 零损失，只损失"已经花掉的归纳成本"。

### 5.2 DDL（已实现）

**以代码为准**：权威定义在 `crates/context/src/store.rs` 的 `Store::init`，下面是它的
忠实摘抄（不是早期草案）。早期草案里的 `watermark` 表没有落地 —— 水位与游标在
`meta(key, value)` 里按处理器各存一行；`atom` 也多了稳定 id 与写入溯源列。

```sql
CREATE TABLE meta (                  -- 水位/游标/跳过记录，按 key 独立推进
  key   TEXT PRIMARY KEY,            -- schema_version | watermark | cursor_offset
  value TEXT NOT NULL                --  | digested_through | log_path | skipped
);

CREATE TABLE atom (                  -- L1：原子记录（近原文的物化视图；不是摘要）
  seq           INTEGER PRIMARY KEY, -- 就是事件 seq，天然稳定 ID
  id            TEXT    NOT NULL UNIQUE, -- 事件自带的稳定 id
  time_ms       INTEGER NOT NULL,
  kind          TEXT    NOT NULL,    -- tool.call / tool.result / message.appended / …
  role          TEXT,                -- user | assistant | tool（消息类才有）
  tool          TEXT,                -- 工具名的确定性提取
  ok            INTEGER,             -- tool.result 的成败
  text          TEXT    NOT NULL,    -- 去掉噪音后的可检索文本（§5.3）
  content_hash  INTEGER NOT NULL,    -- 去重
  is_write      INTEGER NOT NULL DEFAULT 0,
  call_id       TEXT,                -- tool.call 与 tool.result 配对
  write_attempt TEXT                 -- 写入尝试的来源（"先失败后通过"要用）
);
CREATE INDEX atom_kind ON atom(kind);
CREATE INDEX atom_tool ON atom(tool);
CREATE INDEX atom_hash ON atom(content_hash);
CREATE INDEX atom_time ON atom(time_ms);

CREATE TABLE entity (                -- 路径/模块/符号/工具/命令/测试/错误码
  id   INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,                -- 规范化后的名字（§5.4）
  UNIQUE(kind, name)
);
CREATE TABLE atom_entity (
  seq       INTEGER NOT NULL REFERENCES atom(seq),
  entity_id INTEGER NOT NULL REFERENCES entity(id),
  PRIMARY KEY (seq, entity_id)
);
CREATE INDEX atom_entity_e ON atom_entity(entity_id);

CREATE TABLE summary (               -- L2/L3；L4 不落库（见下）
  id            TEXT PRIMARY KEY,
  level         INTEGER NOT NULL,    -- 2|3
  scope         TEXT    NOT NULL,    -- episode:<id> | domain:<module>
  title         TEXT    NOT NULL,
  content       TEXT    NOT NULL,
  status        TEXT    NOT NULL,    -- open|done|blocked|known（由程序算）
  producer      TEXT    NOT NULL,    -- deterministic | digest+model:<name>[(failed: …)]
  schema_ver    INTEGER NOT NULL,
  seq_from      INTEGER NOT NULL,
  seq_to        INTEGER NOT NULL,
  superseded_by TEXT,                -- 已定义、尚未被写过（§11.1）
  domain        TEXT,                -- L3 的模块归属，确定性规则（§14 未决项 6）
  tokens        INTEGER NOT NULL,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);
CREATE INDEX summary_level ON summary(level, scope);
CREATE INDEX summary_range ON summary(seq_from, seq_to);

CREATE TABLE summary_inputs (        -- 派生血缘：让 §4.2.1"不许链式总结"可检查
  summary_id TEXT    NOT NULL,
  input_id   TEXT,                   -- 子层摘要；NULL 表示原始事件区间
  seq_from   INTEGER,
  seq_to     INTEGER
);
CREATE INDEX summary_inputs_s ON summary_inputs(summary_id);

CREATE TABLE citation (              -- 摘要 → 原始证据（区间，不是逐条）
  summary_id TEXT    NOT NULL,
  seq_from   INTEGER NOT NULL,
  seq_to     INTEGER NOT NULL,
  PRIMARY KEY (summary_id, seq_from, seq_to)
);

CREATE TABLE summary_entity (
  summary_id TEXT    NOT NULL,
  entity_id  INTEGER NOT NULL,
  PRIMARY KEY (summary_id, entity_id)
);
CREATE INDEX summary_entity_e ON summary_entity(entity_id);
```

两点与早期草案的实质差别：

- **L4 不存成 `level = 4` 的行。** 全局地图每次从当前全部 L2 摘要现算
  （`Store::global_map()`，硬上限 400 token）：它必须反映"此刻"，存一行反而要不断判断
  它是否过期。所以 `ContextExplain.memory_layers` 里 L4 的"条数"是 1（有地图）或 0
  （还没有），**不是** `summary_count(4)`（那恒为 0）。
- **没有摘要全文索引表**，理由与测量见 §5.3 与 §14 未决项 4。

### 5.3 `atom.text` 存什么（决定索引质量与大小）

**存"可检索的抽取物"，不存原始 JSON，也不存完整输出**：
`tool.result` 取输出**头部**（上限 `MAX_TEXT_CHARS = 512`，目前是常量、未进配置）；
`tool.call` 取 `tool` + 有代表性的参数（path/command/pattern）；`message.appended` 取
全文但同样有上限。完整正文留在 JSONL 与 `events` 表里，不复制。

**这条限制是被磁盘预算逼出来的**（实测数据见 §14 未决项 3；FTS5 本身则被 §14 未决项 4 否决）：FTS5 trigram 的索引
体积约为原文的 **2.1 倍**（连同正文约 3.2 倍）。若对 `atom.text` 建 trigram 索引，
而 `atom.text` 又 ≈ 日志正文，则一个 10 万事件的会话会为"能 grep 原文"多付**三倍磁盘**，
与"存储尽量简化"直接冲突。所以分层地用不同的检索手段：

| 用途 | 走哪条路 | 成本 |
|---|---|---|
| 找**结论 / 任务 / 模块**（高信噪比，最常查） | `summary` 上的 `LIKE`（逐词 AND） | 4 000 条摘要实测 **3.8–5.6 ms**；无索引体积 |
| 精确过滤（文件、工具、测试、错误码） | `atom_entity` / `atom.tool` / `atom.kind` 索引 | 行存，无放大 |
| grep 式扫原文（兜底、低频） | `atom.text` 上的 `LIKE`，先用 kind/tool/entity 收敛 | **实测 0.2–0.35 ms**（见下） |

**P1 已实现并实测**（`crates/context`，测量代码在
`crates/context/tests/search_cost.rs`，`--ignored --nocapture` 运行；
4 万事件 / 4.4 万原子 / 日志 9.7 MB）：

| 查询 | 耗时 |
|---|---|
| 2 字中文（`层叠`，正是 FTS5 trigram 会静默返回 0 的那种） | **0.30 ms** |
| 3 字中文（`深拷贝`） | 0.31 ms |
| 标识符（`layer_merge`） | 0.22 ms |
| 路径片段（`src/config`） | 0.23 ms |
| 英文常用词（`FAILED`） | 0.26 ms |
| 两个词都命中（`层叠` + `配置`） | 0.32 ms |
| **两个词不可能同时命中**（`层叠` + `FAILED`） | **23.5 ms**（扫不到就停不下来的最坏情况） |

两条结论：①`LIKE` 路径比当初 2 万行 Python 原型测得的 5.1 ms **更快**，
所以"不给原文建全文索引"这个决定站得住；②**唯一毫秒级的形状是"查不到"**，
而它恰恰是要用来回答"这里没有这段历史"的 —— 不能把它优化成一条快速空路径。
摄入约 **8 200 原子/秒**（debug 构建，每实体一次 UPSERT；P3 的 worker 会批量提交）。

即：**全文索引只建在摘要上，原文走 LIKE**。若实测证明 LIKE 不够快，再考虑加一条
char-unigram 派生列（总量约 1.8 倍，且对 `LIKE` 语义无损、无字典依赖）—— 不在 v1。

### 5.4 实体的确定性抽取规则（已实现；**未进配置**）

实现见 `crates/context/src/materialize.rs`（规则写死在代码里，没有配置键）：

| 来源 | 规则 | 实体 |
|---|---|---|
| 工具参数 | `path`/`file`/`pattern` 字段 | `path:src/kernel/turn.rs`，`module:src/kernel` |
| 命令 | 首个非 env token | `command:cargo test` |
| 测试输出 | `test <name> ... ok/FAILED` | `test:<name>` |
| 错误 | `error[E\d+]` / 异常类名 | `error:E0308` |
| 文本 | 反引号内路径/符号、`foo.rs:12` 形式 | `symbol:foo` |
| 用户消息 | 命中模块名/路径 | 对应模块 |

**模块映射是确定性规则**（取路径前 1–2 段，`synthesis::domain_of`）。这是关键设计选择：
**L3 的归属不由模型决定**，因此可预测、可测试、便宜。模型只写**内容**，不选**位置**。
落不到任何模块的摘要进 `inbox`。

**诚实注记（2026-10-14）**：上面"让脏集合能精确计算"这句话，只在"归属是确定性规则"
这个意义上成立；**脏集合本身没有实现** —— 当前 `digest_domains()` 每轮遍历全部 domain
比较内容（§6.4、§11.1），不是 O(增量)。

---

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
  （§4.4 的字段归属表）。
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
- **`title` 不由模型改写**；digest 与 synthesis 的字段归属见 §4.4。
  `producer` 里是**默认模型**（实测已改为 7B，见 §14 未决项 2）。

**`tests` 保留"失败→通过"这个转变**，而不是只留最终结论 —— 因为"先失败后通过"正是
"这个修复是被验证过的"的唯一证据。

**本机实测（决定 S 模式怎么用 0.6B，见 §14 未决项 1）**：
- 直接把**原始事件清单**丢给 `qwen3:0.6b`：它会整段复述到 `rejected[]` 里，并且把
  `status` 猜成 `done`/`open` 乱来 —— **完全不可用**。
- 把输入改成**已归纳的事实句**（"做法一：先深拷贝再合并 → 测试仍然失败，已放弃"），
  只问它"哪个做法被否决了、为什么"：`qwen3:0.6b` 一次就答对了
  （`rejected":["做法一：先深拷贝再合并"]`，理由"测试仍然失败，已放弃"，35 token）。
- 结论：**0.6B 不是不能用，是不能让它做"从原始记录里找结构"这件事** —— 这正是
  digest 该干的活。**给模型的输入质量比模型大小更决定成败**，这也是 §4.4 的机制价值。

### 6.3 切段（episode 边界，确定性）

候选边界：①一条新 user 消息且上一段已有终局结论 ②任务终态事件
（测试全绿 / commit / turn 结束且无 pending）③积压事件数达阈值 ④时间间隔超过阈值。

**实现状态（2026-10-14）**：切段只有两条判据落地（`crates/context/src/digest.rs` 的
`segment()`）：新 user 消息且上一段已有 `SEGMENT_MIN_EVENTS = 4` 条事件，或距上一段
起点超过 `SEGMENT_GAP_MS = 10 min`。②④的"阈值进配置"**没有落地** ——
`[context_engine] episode_events` 虽被解析，但还没有被任何一段代码读取（§11.1）。
可重放这一条有测试保证（同输入必得同边界）。

### 6.4 上行触发

**实现状态（2026-10-14）**：

- **L3**：某模块的 L2 数达到 `DOMAIN_ROLLUP_EPISODES = 3`（**不是草案里的 8**，
  见 `crates/context/src/synthesis.rs`）时派生该模块的 L3；内容未变则不写库。
- **L4**：`Store::global_map()` 每次**从当前全部 L2 现算**（不落库、不做增量判脏）。
  这比"任一 L3 变化才重算"更简单，也总是反映此刻；代价是每次 `refresh()` 都要扫一遍
  L2 摘要——实测在 4 000 条量级仍是毫秒级（§14 未决项 4）。
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

## 7. 前台合成与 `recall`

### 7.1 前台怎么变

| 分类 | 今天 | 改造后 |
|---|---|---|
| system / tools / rules / skills | 不变 | 不变 |
| L4 地图 | 不存在 | 新 `Category::CognitiveMap`（`Lifetime::Session`, `pinned`），按 `replace(id, text)` 原地更新 |
| 历史消息 | 累积到 ceiling → 清理/丢弃 | R 模式同今天；S 模式**只清工具输出**，不丢消息 |
| recall 结果 | 不存在 | **不另记账本条目**：答案作为普通 `tool.result` 进入历史，由 history 分类计费（原因见 §7.2 要点 8） |

`Category::Retrieval` 这个分类**存在于枚举里但 `recall` 不使用它** —— 早期设计想让检索
结果以 turn 生命期参与预算竞争，实现时发现那会把同一批字节**双重计入**上下文预算。
`Lifetime::Turn` 的账本条目目前仍被预算淘汰逻辑使用，只是不由 `recall` 产生。

L4 必须**有 token 上限**：当前是常量 `L4_MAX_TOKENS = 400`（`synthesis.rs`），
`[context_engine] l4_max_tokens` 虽被解析但**还没有被读取**（§11.1）。R 模式与 S 模式
共用同一个确定性 L4；草案里"R 模式的 L4 更短（200）"没有实现。

### 7.2 `recall` 工具（唯一检索入口）—— 已实现

```jsonc
// 调用 1：定位（摘要优先，摘要为空才回落到原文事件）
{"query": "层叠 配置", "entity": "src/config", "tool": "edit_file", "level": 2, "limit": 8}
→ memory (summaries; expand one before relying on it):
  - [ep-00000118-00000143] L2 修复 config 层叠失效 (done) seq[118-143]
    L2 · … | 状态：done | 改动文件：src/config/merge.rs | …

// 调用 2：展开（摘要正文 + 它引用的原始事件，一次到位）
{"expand": "ep-00000118-00000143"}
→ summary [ep-00000118-00000143] L2 …（正文）
  evidence (the original events this summary rests on):
  [121] tool.result FAILED run_command: test layer_merge ... FAILED
```

设计要点（实现后的版本）：

1. **`expand` 强制同时给原文**，不只给摘要正文 —— 从机制上防止"把摘要当真相"。
2. `expand` 是**纯读**（摘要已物化），不触发模型，因此不引入延迟与抖动。
3. 被清掉的工具输出仍可从日志取回（evidence 读 `atom`，读的是抽取物与 `seq`）。
4. `limit` 默认 8、封顶 32；整条答案有**硬字符上限**（`RECALL_MAX_CHARS = 4 800`），
   截断时显式标注 —— 一个被悄悄截短的检索结果会读成"这就是全部"。
5. 关键词**逐词 AND**，每个词独立匹配（不走 FTS5，从根上避开 `<3` 字静默返回 0）。
6. **搜不到是一个正常答案，不是错误**：返回"没有匹配，但别据此断定什么都没发生"。
   返回错误会教会模型"问过去的事是坏的"。
7. `recall` **绕过 PDP 且不产生审批**：它只读本次会话自己的记录，而这份记录模型
   已经完整拥有过；没有可以门禁的东西。调用本身仍然是一条 `tool.call` 事件。
8. `recall` **不额外记一条 `Category::Retrieval` 账本条目**：答案会变成 tool-result
   消息，账本已经在计这些 token；再记一条会让同一批字节被双重计入上下文预算，
   而那是人用来判断"这个会话花不花得起"的那个数字。
9. `recall` **只在引擎开启时出现在工具表里**：模型能调但答不出的工具，比没有这个
   工具更糟 —— 它会教会模型"问过去没用"。

### 7.3 系统提示的纪律（一小段，不是长篇）

> 关于过去的工作，先 `recall` 再回答；任何"已验证/已修复"的断言都必须带 evidence 区间。
> 摘要是派生索引，可能与原文不符 —— 有疑问时展开原文，以原文为准。

---

## 8. 失效模式与对策（本设计的核心风险表）

| # | 失效模式 | 后果 | 对策（机制，非约定） |
|---|---|---|---|
| 1 | **漂移**：L4 越抽象越偏离事实 | 模型自信地记错项目状态 | ①**禁止链式总结**：每层只从当前子层重派生，旧版摘要不进输入集合（§4.2.1，`summary_inputs` 可机械检查）②引用区间 + `superseded_by`（**列在、从未写入**，见 §11.1）+ `expand` 强制带原文（§7.2 要点 1） |
| 2 | 高层摘要吞掉关键细节 | 决策依据消失 | digest 保底（§6.2）+ L1 原子记录 + 关键词索引按需回填 |
| 3 | 摘要被当事实 | 错误结论在会话内传播 | 系统提示纪律 + `expand` 强制带原文 + 摘要标注 `producer` 与 `status` |
| 4 | 后台拖慢交互 | 体验变差 | 单飞行 + 窗口预算 + 前台优先 + 可关闭（§4.6）；走本机 HTTP 端点时模型在别的进程里 |
| 5 | 归纳成本失控 | 电费/延迟爆炸 | 增量水位 + 脏集合 + 上行收敛 + 每窗口硬预算 |
| 6 | 小模型质量不足（编造决策 / 漏掉状态） | 归纳无用甚至有害 | **忠实度测试作为采纳门槛**（§14 未决项 2）：10 份真实 digest + ground truth 计编造/漏项；过不了就升档或退回 R 模式 |
| 7 | 存储膨胀 | 磁盘增长 | **不建任何原文/摘要全文索引**（§5.3、§14 未决项 4）；`atom.text` 是抽取物且有上限；原文不复制；`vacuum`/保留策略 |
| 8 | 索引损坏/版本升级 | 记忆不可用 | 可丢弃可重建是硬要求；`schema_ver` + 重建路径有测试 |
| 9 | 多进程并发写同一 DB | 损坏 | 一个会话内**两个连接**（前台派生 + 后台富化）：WAL + `busy_timeout`（输者等赢家的短事务）；进程之间仍是"一个会话一个写者"，没有做"检测到其他写者则让位"。注意 WAL 下 checkpoint 前比较体积无意义 |
| 10 | 摘要掩盖了"被否决的方案" | 重蹈覆辙 | L2 synthesis 的**必填字段**：`rejected[]`、`open_questions[]` |
| 11 | 关键词查询静默返回 0 | 模型以为"没有这段历史" | 关键词逐词 AND（§7.2 要点 5）；不走 FTS5，从根上避开短词陷阱 |
| 12 | **输入里没有的信息，模型只能编或弃权** | 要么记忆变薄，要么记忆变假 | digest 必须携带**尝试序列**（每次改动 + 紧随其后的验证结果，§6.2）；能算的字段一律由程序算，绝不让模型"抄一份它读得到的清单" |
| 13 | 静默降级（模型连不上、schema 版本不对） | 用户以为记忆在跑，其实没有 | `memory_degraded` 记录原因（引擎级失败 + "N 段退回 digest"），TUI F2 面板与状态栏 `mem ⚠ degraded` 显示（**已实现 2026-10-14**，见 §11.1） |

---

## 9. 事件种类（**尚未落地**）

后台工作与摘要变更都必须是**可见事件**，否则 ADR-0006 的"没有隐藏的注入"就破了。
拟新增四类（`crates/schema/protocol.toml` → `scripts/gen-schema.py` 生成）：

> **实现状态（2026-10-14）：这四类事件目前一个都没有。** `protocol.toml` 里没有
> `memory.*` 事件，引擎也从不写日志 —— 它只读会话日志、只写自己的 sidecar。
> 现状是：水位在 `meta` 表、摘要在 `summary` 表、worker 统计在进程内的
> `ContextExplain.memory_worker`，**都不是事件**。所以"摘要变更可审计、水位不是
> 暗状态"这条承诺**还没有兑现**；这是 §11.1 缺口表里的一条。`materialize.rs` 已经
> 认得 `memory.summary.updated` / `memory.superseded` 两个 kind（遇到时按可检索事件
> 物化），但那只是为将来预留的读取路径，不是有人在写它们。

| 事件 | 字段（要点） | 为什么必须是事件 |
|---|---|---|
| `memory.l1.materialized` | `seq_from`, `seq_to`, `atoms`, `entities`, `tokens` | 让"哪些事件已被索引"可审计、可续跑；水位不是暗状态 |
| `memory.summary.updated` | `id`, `level`, `scope`, `producer`, `model`, `tokens`, `citations` | 摘要进入了前台上下文，它出现在日志里才算可解释 |
| `memory.superseded` | `id`, `by`, `reason` | 结论被取代是**历史事实**，不能静默覆盖（§4.1） |
| `memory.worker.state` | `state`(idle/working/deferred), `pending`, `watermark`, `model_calls` | 后台花了多少、为什么停，是可查询的账 |

约定：这四类都 `ignorable = false`（核心事件）—— 一个读不懂摘要事件的读者，
不该假装自己理解了这次会话的上下文是怎么被组织的。摘要**正文**不进事件，
只进 `summary` 表：日志是真相，但真相是"摘要存在且指向这些区间"，不是摘要文本
（全文进日志会让日志体积翻倍，而它对重建没有增量价值）。

---

## 10. 与现有 ADR 的关系

| ADR | 关系 |
|---|---|
| 0003 JSONL 真相 + SQLite 派生 | **加强**：新增的是一套派生记忆，仍然可丢弃重建 |
| 0004 微内核、固定核心 | 引擎放**独立 crate**（拟 `crates/context`），内核只调用其只读接口；摘要不进入内核的决策逻辑 |
| 0006 上下文引擎 / 账本 | **修订 §4**（压缩顺序与可逆性）；新增 `Category::CognitiveMap` 与 `Ledger::replace` |
| 0008 配置分层 | 新增 `[context_engine]` 与 `[summarizer]`：全局开关是 narrow-only |
| 0013 Rust 单二进制 | 小模型后端必须可选；默认构建不引入 C/C++ 工具链与网络下载 |
| 0015 两层树 | 引擎按会话内事件树工作；跨会话复用（T-10 知识而非会话）是**后续扩展**，见 §12 |
| 0014 会话导出/导入 | 派生记忆不进 bundle（只导出 JSONL）；导入后按需重建，重建是纯派生操作。这正好检验"可丢弃可重建"这条硬要求 |

`loong-index`（`Index::search` 的 `LIKE` 路径）在 L1 索引就位后成为旧路径，
本设计不删除它，但新功能一律走新引擎。

---

## 11. 配置（已接线部分 + 仍在草案部分）

**owner 决定（2026-10-13）：小模型后端先用本机 Ollama。** 默认模型已按 2026-10-14 的
忠实度重测改为 `qwen2.5:7b-instruct`（见 §14 未决项 2）。
**真正被解析的键**（`crates/config/src/lib.rs`，2026-10-14 核对）：只有下面这些。
`RawContextEngine` / `RawSummarizer` 都是 `deny_unknown_fields`，所以写了未列出的键会
**报错**，不会被静默忽略 —— 早期草案里的键照抄进配置会直接启动失败。

```toml
[context_engine]
enabled = true                 # narrow-only。关掉 = 改造前的行为
mode    = "semantic"           # record | semantic
episode_events = 32            # ← 被解析，但**当前无人读取**（切段仍用常量，§6.3）
l4_max_tokens = 400            # ← 被解析，但**当前无人读取**（L4 上限仍是常量，§7.1）

[summarizer]                   # 仅 mode = "semantic" 时被读取
enabled   = true               # narrow-only 开关：企业层可置 false，用户层不能放宽
kind      = "ollama"           # ollama（原生 /api/chat）| off
base_url  = "http://127.0.0.1:11434"   # 仓库层不得设置（ADR-0008）
model     = "qwen2.5:7b-instruct"      # synthesis 的语义字段（默认模型，§14 未决项 2）
window_calls = 4               # 每个空闲窗口最多几次归纳（narrow-only，取最小）
window_tokens = 6000           # 每个空闲窗口最多多少 token（估算；narrow-only，取最小）
```

**仍在草案、尚未接线的键**（写进配置会 `unknown field` 报错）：`store`、
`episode_gap_secs`、`domain_rollup_episodes`、`recall_limit`、`recall_max_tokens`、
`summarizer.model_fast`、`summarizer.think`、`summarizer.timeout_ms`、
`summarizer.on_error`。它们描述的行为目前是写死的：`think = false` 与 60 s 超时在
`OllamaSummarizer` 里，失败一律 degrade 到 digest，存储只有 sidecar 一种，recall 上限是
`RECALL_MAX_CHARS = 4800`（常量），domain rollup 阈值是 `DOMAIN_ROLLUP_EPISODES = 3`
（常量）。这些都列在 §11.1。

`window_calls` / `window_tokens` 是**每个空闲窗口**的硬预算（§4.6），不是"每个步边界"，
并且是 ADR-0008 的 narrow-only 键、**取最小值**：更高层可以把后台的花费压小，更低层
不能把它放大。窗口用尽即休眠，下个窗口继续；一次失败计入调用预算，且（对同一个模型）
不再重问该段。

**缺省即关**：没有任何一层说过 `context_engine.enabled`，就按改造前的行为走。

**怎么开启（不用手写文件）**：`loongagent local semantic on [模型]` 会把上面这些键写进
`~/.loongagent/local.toml`（只对 `loongagent tui --local` 生效）；`local semantic off`
关掉，`local semantic` 看当前状态与模型是否已 `ollama pull`。手写本节只用于受管模式
（`~/.loongagent/profile.toml`）或按仓库收紧。

**为什么默认是 `kind = "ollama"`（原生协议）而不是 `openai` 兼容端点** —— 本机实测：
`/v1/chat/completions` 会**忽略 `think:false`**，把推理过程塞进 `reasoning` 字段并计入
`completion_tokens`，一次 200 token 的预算全被推理链吃掉，`content` 只剩半句话。
原生 `/api/chat` 带 `"think": false` 时格式正确、无推理开销。这是必须记在配置文档里的坑，
否则"为什么摘要都是半截的"会变成一个查不出来源的问题。
（`providers` 现有的 `OpenAiCompat` 因此**不适合**直接当摘要后端 —— 它没有 `think` 开关。）

窄化规则（ADR-0008）：企业层可以**强制** `enabled=false`、`mode="record"`、
指定 `summarizer.kind` 与可用模型；用户层不能放宽。`window_calls`/`window_tokens`
同为 narrow-only，取最小值。

CLI/TUI 面（**2026-10-14 的实际情况**）：

```
loongagent local semantic                 # 已实现：看开/关、provider、模型、端点、是否已 pull
loongagent local semantic on [模型]        # 已实现：打开语义模式并选摘要模型（写 local.toml）
loongagent local semantic on --record      # 已实现：只做确定性派生，不调用模型
loongagent local semantic off              # 已实现：关闭
loong context status               # 已实现：水位、实体、各层条数、producer 分布
loong context verify               # 已实现：引用完整性校验（§4.1）
loong context rebuild              # 已实现：丢弃并重建派生库
F2 面板                            # 已实现：账本分类 + 记忆状态（见 §11.1）

loong context build [--all]        # **草案，未实现**
loong context explain              # **草案，未实现**（L4 全文只出现在请求里）
/context                            # **草案，未实现**（TUI 用 F2）
```

`local semantic` 写在 `~/.loongagent/local.toml`（`local add` 的同一个文件），因此只对
`loongagent tui --local` 生效；它用与 `local add` 相同的"文本手术"而不是重新序列化，
文件里的注释和其他键不会被抹掉。摘要 provider 目前只有 `ollama` —— 见 §14 未决项 1 的
实测（OpenAI 兼容的 `/v1` 忽略 `think:false`），写别的 provider 会被具名拒绝。

（`loong context status` 里的 L4 计数一度用 `summary_count(4)`，那恒为 0；已改为与
`memory_layers` 同一口径 —— L4 不是存储的行，见 §5.2。）

`ContextExplain` 增加（**已实现，2026-10-14**）：`memory_mode`（引擎关时为空，否则
`record`/`semantic`；"开/关"只认它，不认账本里有没有 `cognitive_map`）、
`memory_summarizer`、`memory_degraded`（降级原因，空=健康）、
`memory_layers[]`（L1–L4 的条数与 token；L4 是"有地图=1 / 还没有=0"）、
`memory_worker`（state / windows / model_calls / enriched / failed / pending /
deferrals / last_error）。前台"为什么这句话在这里"的能力保持。
TUI 在 F2 面板渲染它们，并在降级时于状态栏常驻 `mem ⚠ degraded`。

---

## 11.0 TUI：`@` 文件引用（2026-10-14）

输入框里打 `@` 会列出工作区文件，`↑/↓`（或 `Tab`/`Shift-Tab`）选择，`enter` 插入，
`esc` 取消。它插入的是**路径文本**（`@src/main.rs`），**不读文件内容**。

两条理由，第二条是决定性的：

1. 模型本来就有 `read_file` 工具，路径正是它需要的；贴内容会把它能自己取、能改、
   能引用的东西复制进对话。
2. **上下文是这套设计里最稀缺的资源**。一个自动附带文件内容的补全，会花掉用户
   从未选择花的 token，而且账本里说不出它们去了哪。**路径是引用，内容是预算决策。**

至于 *agent* 是否应该把 `@path` 也展开成一次读取 —— 那是内核的问题，不是键位的问题，
本模块不作决定。

实现要点（`crates/tui-app/src/mention.rs`）：

- 文件清单来自 `git ls-files --cached --others --exclude-standard`，所以 `target/`、
  `node_modules/` 以及项目自己 ignore 的东西**天然不会出现**；没有 git 时退化为有界遍历。
  索引**只读一次**：每次按键都遍历目录会让打字本身变慢。
- 排序按**匹配位置**（basename 前缀 > basename 出现 > 路径出现 > 子序列），
  不是模糊打分：`main` 要能把 `src/main.rs` 排在 `src/vendor/main_helper.rs` 前面，
  这个判断够了；没人能预测的打分函数比可预测的更糟。
- picker 是**从文本推导出来的**，没有独立的开关；`esc` 用一个"已忽略"标记覆盖它，
  否则下一次按键会把它推导回来（**这是测试抓到的真 bug**）。
- 键位**排在所有 Escape/方向键分支之前** —— 否则 catch-all 的 `Esc`（退出程序）
  会把它吞掉（**这也是测试抓到的真 bug**）。

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
| **`memory.*` 事件一个都没落地** | 仍缺（**2026-10-14 审计发现**） | §9 的四类事件（`memory.l1.materialized` / `memory.summary.updated` / `memory.superseded` / `memory.worker.state`）在 `protocol.toml` 里不存在，引擎也从不写日志。摘要变更因此**不能从日志审计**，水位与 worker 统计只在 `meta`/进程内。ADR-0017 的 Consequences 曾把它写成已新增，现已在那边标注"尚未落地" |
| **`superseded_by` 从未被写过** | 仍缺（**2026-10-14 审计发现**） | 列存在、读取路径存在，但 `replace_overlapping` 对过期摘要执行的是 `DELETE`，不是"标 `superseded_by` 保留"。ADR-0017 §6 的"旧摘要标 superseded_by，不静默覆盖历史"**没有兑现**；当前语义是"重叠即重派生" |
| **`l4_max_tokens` / `episode_events` 解析了但没人读** | 仍缺 | 两个键进配置后会合并进 `ContextEngineSettings`，但 L4 上限仍是常量 `L4_MAX_TOKENS = 400`、切段仍是 `SEGMENT_MIN_EVENTS = 4`。一个"接受但无效"的键比一个报错的键更危险，所以单列（§7.1、§6.3） |
| **一批草案键根本没接线** | 仍缺 | `store` / `episode_gap_secs` / `domain_rollup_episodes` / `recall_limit` / `recall_max_tokens` / `model_fast` / `think` / `timeout_ms` / `on_error` —— `deny_unknown_fields` 下写入即报错（§11） |
| **L3 脏集合 / 上行最小间隔** | 仍缺 | §6.4 的"按实体标脏 + 只重算脏模块 + 最小重算间隔"没有实现；`digest_domains()` 每轮遍历所有 domain 比较内容，L4 每轮从全部 L2 现算。规模上目前是毫秒级，但这是"新内容只处理一次"承诺里唯一没有做到 O(增量) 的部分 |
| **CLI `context build` / `context explain` / TUI `/context`** | 仍缺 | §11 里列过；实际只有 `status`/`verify`/`rebuild` 与 F2 面板 |

### 这两件怎么落的（2026-10-14 已实现）

**空闲窗口批量归纳**。`refresh()` 只做**确定性**派生（`sync` / `digest_pending` /
`digest_domains`）并唤醒后台；模型调用在一条独立线程上，按
`window_calls` / `window_tokens` 的硬预算开一个**空闲窗口**，窗口之间休眠，
`Foreground::is_busy()` 为真时让出。几个决定：

- **第二个 `Store` 连同一个 sidecar**，不是 `Arc<Mutex<Store>>`。共享一把锁会让前台
  等在后台的**模型调用**上，正是 §4.6 要避免的失败；两个连接只在 SQLite 的短写事务上
  串行（WAL + `busy_timeout`），模型请求期间没有事务在手。实测：`Store::open` 设置的
  5 s `busy_timeout` 能让输掉竞争的写者等赢家提交后继续；`rusqlite` 自己恰好也有 5 s
  默认值，但我们把它写出来 —— 保证是这套设计的，不是驱动的默认值
  （`crates/context/tests/two_writers.rs` 里有对照组：超时为 0 时同样的写在 300 ms
  的锁下**立即**报 `database is locked`）。
- **线程归属：引擎（`crates/kernel/src/context_engine.rs`），不是 server 也不是 CLI。**
  两者都通过同一个工厂建 kernel（`crates/cli/src/main.rs`），`session.new` 换掉的是
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
  worker 永远到不了 §4.5 要求的稳态。换一个模型名是另一个问题，会重问。失败也**计入**
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

---

## 11.2 模型不写文件、只在聊天里贴代码（2026-10-14 修）


**症状**：用本地模型让它写一个单页 HTML 游戏，它把整个文件贴在回复里、让你自己保存；
有时又说"我会创建 typing.html"却什么都没做。写诗到 txt 反而成功率高。

**根因（有证据，不是猜的）**：

1. **从你自己的会话日志里确认**：`~/.loongagent/projects/…/77bahbba.jsonl` —— 6 条
   assistant 消息、**0 次 tool.call**。回复里明确写着「请按照以下步骤操作：1. 打开文本
   编辑器 2. 复制粘贴 3. 保存为 typing.html」，后来又写「请允许我进行操作：```
   write_file typing.html - …```」——**它把工具调用当散文写了出来**。
2. **我们的系统提示里从来没有提过"用工具改文件"**。它只说了 `skill` 工具怎么用，
   连工具名字都没列。小模型是按提示里怎么写的工作方式去做的；一个从不提工具的提示，
   得到的就是散文而不是动作。
3. **测试 Ollama 的 `/v1` 证明它有能力**：三个模型在部分请求里都正确发出了
   `tool_calls: write_file`，参数也对。所以这不是"模型不支持工具调用"，
   而是**不稳定**。
4. **为什么短内容成功率高**：写诗/txt 的目标内容短，工具参数的 JSON 短，模型更容易
   一次性写对；单页游戏是长文件，工具参数很长，模型更容易退回"贴出来"。

**修法（两层）**：

- **把工作方式写进系统提示**（`seat_system_prompt`）：列出工具名，并明确禁止
  "把文件内容贴进回复让人自己保存"，要求"声称创建了文件就必须是真的"。
- **一轮纠偏**（`run` 里）：如果这一轮**没有调用过任何工具**、回复又像"贴了整个文件"
  （带文件名或语言的代码块、或散文形式的 `write_file …`），就插入一条指令让它
  **真的调用 `write_file`** 再试一次；**只纠偏一次**，第二次仍贴就直接接受并让用户
  看到结果 —— 一个和模型无限争论的 harness 比一个让用户看见它拒绝的更糟。
  这条纠偏**不写进事件日志当作用户消息**：那是 harness 在纠偏，不是用户说的话。

**我没有证明的**：系统提示改动的效果。做了一次 A/B（各 1 次采样）：7B 前后都调用工具、
3B 从"不调用"变成"调用"、0.6B 从"调用"变成"不调用"。**单次采样说明不了任何事**，
要下结论得跑多次。真正确定的是机制和上面两层修复。

**第二个形状：把工具调用**打印**成文本（2026-10-14 补）**

修完"贴文件"之后又实测到同一失败的另一副面孔。日志
`…/customcodingagentbench/47x75pvh.jsonl` seq 76–78：同一个任务，上一轮用
`qwen3.7-flash` 走的是正常路径（`read_file` → `write_file` → 落盘 2364 字节）；
换成 `qwen2.5:7b-instruct` 后，它把调用写进了 **`content`**：

```
portun
{"name": "write_file", "arguments": {"content":"# 《2026年Q3…》报告摘要…", "path":"…/markdown.md"}}
</tool_call>
```

于是**一条 `tool.call` 事件都没有**，turn 正常结束、回复读起来像成功了，工作区却
没变。三点值得记：

1. **它不是"贴文件"**：旧检测器只认带语言/文件名的代码块和 `write_file …` 散文，
   这条内容两者都不匹配，所以**没有触发纠偏** —— 机制在，形状没想到。
2. **内容甚至从词中间开始（`portun`，是 `Counterpoint` 的尾巴）**。那是 provider
   返回的 `content` 本身如此（我们的 SSE 读取按整行缓冲，且 chunked 截断的修复已在
   `loong-http` 里，见 `BodyReader` 的注释）。**所以检测不能"从头解析 JSON"，只能扫描**：
   文本可能两头都被切掉。
3. **同一个模型并不是不会调用**：本机对 Ollama `/v1` 的三次短上下文采样都是干净的
   `tool_calls` delta、`content` 为空。这是 §11.2 开头那条结论的又一例 —— **不稳定**。

**原始样本（真实抓取，`--debug-model`）** —— 差别就在通道，不在措辞：

```jsonc
// ✅ deepseek-flash：调用走 tool_calls 字段（服务端也这么认为）
{"finish_reason": "tool_calls",
 "content": "report.txt has one finding. Writing the summary.",
 "tool_calls": [{"name": "write_file",
                 "arguments": {"path": "out.md", "content": "# report.txt 总结…"}}]}

// ✅ qwen2.5:7b-instruct（短上下文，同一个模型）：同样走 tool_calls
{"finish_reason": "tool_calls",
 "content": "I'll start by checking the workspace…",
 "tool_calls": [{"name": "shell", "arguments": {"command": "ls -la"}}]}

// ❌ qwen2.5:7b-instruct（长历史复现）：调用被"打印"进 content，tool_calls 为空
{"finish_reason": "stop",
 "content": "ington\n{\"name\": \"write_file\", \"arguments\": {\"path\": \"markdown.md\", \"content\": \"…\"}}",
 "tool_calls": []}

// ❌ 同一模型的另一种退化：直接声称做完，连打印的调用都没有
{"finish_reason": "stop",
 "content": "…最新报告总结已写入 `markdown.md`，内容如下：…",
 "tool_calls": []}
```

判据是 `finish_reason`：`tool_calls` = 服务端认为这是调用；`stop` + content 里出现 JSON =
模型把调用打印成了文本。**注意两个 ✅ 与两个 ❌ 用的是同一个 harness、同一个请求形状** ——
harness 没变，变的是模型在压力下的输出通道。长历史复现时连"词中间开始"（`ington`，
`Counterpoint` 的尾巴）都一模一样地重现了，所以那不是 SSE 读串了。

**补的修法**（同样只纠偏一次）：
- `leaked_tool_call()`：窄匹配 `"name"` + `"arguments"` 且名字是已知工具，或显式的
  `<tool_call>` 标记；**在文本任意位置扫描**，不要求它是合法 JSON、也不要求从头开始。
  它返回工具名，纠偏就说"你把 `write_file` 调用写成了文本…现在真的发出这个调用"，
  而不是不管模型想调什么都让它 `write_file`。
- **守卫按工具、不按轮次**：旧守卫是"这一轮调用过任何工具就不纠偏"。读取文件算调用过，
  但**不是**模型随后打印的那个工具 —— 那种情况下文件一样不会写。现在记录本轮真正跑过
  的工具名：打印的调用若指向**没跑过**的工具就纠偏；若指向跑过的（模型事后复述调用），
  就不纠偏，避免重复写一遍。
- 系统提示相应加一句：不要输出 `<tool_call>` 标签或 `{"name": …}` 形式的 JSON 文本。

测试：`a_printed_tool_call_is_recognised_and_prose_is_not`（含"讨论工具"的负例）、
`a_model_that_prints_a_tool_call_gets_one_chance_to_emit_it`（用日志里的原文形状）、
`a_read_then_printed_write_is_still_corrected`、`printing_a_call_for_a_tool_that_already_ran_is_not_corrected`。
去掉 `leaked_tool_call` 即可看到第二个测试失败 —— 纠偏不是摆设。

**怎么自查"模型到底发了什么"**：`--debug-model`（或 `LOONGAGENT_DEBUG_MODEL=<文件>`）
把每次交互按行写成 JSON 到 `<session>.models.jsonl`，字段有 `finish_reason`、完整
`content`、解析出的 `tool_calls`、用量，以及产生它的 request。判据是 `finish_reason`：
`tool_calls` = 服务端认为这是调用；`stop` + content 里是 JSON = 模型把调用打印成了文本。
它**不进事件日志** —— 日志是记录（ADR-0003），原始 provider 体是诊断数据。这是本节两个
失败形状与"模型能力"之间的区别第一次可以**直接看**，而不是靠猜。

---

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
| **P1** 真相先可检索 | ✅ **已实现（2026-10-13）** | `crates/context`：L1 物化（确定性抽取 + 内容哈希去重）、实体与关键词索引、增量摄入水位、`rebuild`/`verify` | 按关键词/路径/模块找回任意历史事件；删库重建结果一致；2 字中文可命中（实测表见 §5.3） |
| **P2** 派生摘要可读 | ✅ **已实现（2026-10-13）** | L2 的**确定性一半**：切段、digest（改动文件/命令/测试成败转变/状态）、`summary`/`citation`/`summary_entity`/`summary_inputs` 落库、`recall`+`expand` 只读路径 | 任意 L2 都能展开到原文；引用区间全部有效且落在自身区间内；派生血缘记录为「原始事件区间」（非另一条摘要）；12 个测试覆盖 |
| **P3** 后台常态 | ✅ **已实现（2026-10-13）** | 字节 tail（`tail.rs`）、增量 `sync`、`digest_pending` 补积压、`worker::step`/`run_activation`（前台让出 + 预算 + 统计）、截断自动重建 | 空闲看待机 27 µs；追加一个任务 8.4 ms 且不随日志增长；前台忙时 `Deferred` 且不读日志；**增量派生 == 全量重建**（有断言） |
| **P4** 前台变浅 | ✅ **已实现（2026-10-13）** | L3 域地图（确定性 rollup）+ L4 全局地图（硬上限 400 token）+ 内核 `Category::CognitiveMap`、`Ledger::replace`、`refresh_map`；**引擎开启时不再丢弃任何交换** | L4 是**独立 system 消息**、置于系统提示之后、只一条且原地刷新；引擎开启的长会话 `dropped_messages == 0` 且首轮请求仍在；引擎关闭时行为与改造前**完全一致**（有对照测试） |
| **P5** 语义层可选 | ✅ **已实现（2026-10-13）** | `Summarizer` trait、Ollama 原生客户端（`/api/chat`、`think=false`、JSON schema）、`Store::synthesize`（**纯增量**：合并而非覆盖）、**`recall`/`expand` 工具**（§7.2）、`[context_engine]`/`[summarizer]` 的 `Profile` 解析（含 ADR-0008 权威：企业层可强制 `enabled=false`，用户层不能放宽；仓库层不得选 `summarizer.base_url`/`model`）、忠实度测试 | 未装模型 = R 模式行为（`NoSummarizer`）；装了 = 只补 `rejected`/`why`/`open_questions`/`conclusion`，**计算字段与引用一律不可被模型改写**（6 个测试）；**忠实度实测见 §14 未决项 2**：0.6B 编造 0、3B 编造 3；`recall` 有 6 个内核测试；`Profile` 侧 5 个测试；`ContextEngine` 测试（R 模式不碰模型、semantic 在后台并落库、模型不可达只降级那一段、未知 kind 失败关闭） |
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
   | **0.6B 做不了"从原始记录里找结构"** | 把事件清单直接丢给它，`rejected[]` 变成整段复述，`status` 乱填 | digest 必须先做（§6.2）；模型的输入是**归纳好的事实句**，不是原始事件 |
   | 但 **0.6B 能做"单点语义判断"** | 输入改成事实句、只问"哪个做法被否决、为什么"，一次答对（35 token） | 两档模型：难字段用 3B，单点字段可用 0.6B |
   | `qwen2.5:3b` 也会错**算得出来的字段** | `conclusion`/`rejected` 基本正确，但 `status` 填成 `open`（实际 `done`） | **`status` 等字段一律由程序算**（§4.4） |

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
     不需要按字段分流两个模型；配置里的 `model_fast` 会被拒绝（§11）。
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

     这正是 §5.3 把全文索引**只建在摘要上**、原文走 `LIKE` 的原因。
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
   覆盖"**没有实现**，配置里也没有这个键（§11）。粒度要改是"一个函数 + 一次重派生"，
   见 `synthesis.rs` 顶部注释。

7. ~~前台是否注入 L3~~ **P4 已按"L4 常驻 + L3/L2 按需"落地**：L4 硬上限 400 token
   常驻，L3 与 L2 走 `recall`/`expand`。理由是 L4 的边际成本是常数，而 L3 的数量
   随模块数增长 —— 常驻 L3 会让"最浅表上下文总是最精简"这条一开始就不成立。
   若将来要常驻当前任务的 L3，应是**注入一个、随任务切换**，而不是全部注入。

8. **跨会话复用**（T-10）是否纳入路线图：同项目的历史会话摘要天然是团队知识。
   ADR-0010 的立场是知识走 git，不进共享会话 —— 这个方向可以是一条独立 ADR。
