# 数据模型

Language: zh

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0006](../decisions/0006-context-engine.md) · [ADR-0017](../decisions/0017-layered-context.md)
- **Code:** `crates/context/src/store.rs`, `crates/context/src/materialize.rs`
- **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`

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
  superseded_by TEXT,                -- 已定义、尚未被写过
  domain        TEXT,                -- L3 的模块归属，确定性规则（[rollout.md](rollout.md) §14 未决项 6）
  tokens        INTEGER NOT NULL,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);
CREATE INDEX summary_level ON summary(level, scope);
CREATE INDEX summary_range ON summary(seq_from, seq_to);

CREATE TABLE summary_inputs (        -- 派生血缘：让 [principles.md](principles.md) §4.2.1"不许链式总结"可检查
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
- **没有摘要全文索引表**，理由与测量见 §5.3 与 [rollout.md](rollout.md) §14 未决项 4。

### 5.3 `atom.text` 存什么（决定索引质量与大小）

**存"可检索的抽取物"，不存原始 JSON，也不存完整输出**：
`tool.result` 取输出**头部**（上限 `MAX_TEXT_CHARS = 512`，目前是常量、未进配置）；
`tool.call` 取 `tool` + 有代表性的参数（path/command/pattern）；`message.appended` 取
全文但同样有上限。完整正文留在 JSONL 与 `events` 表里，不复制。

**这条限制是被磁盘预算逼出来的**（实测数据见 [rollout.md](rollout.md) §14 未决项 3；FTS5 本身则被 [rollout.md](rollout.md) §14 未决项 4 否决）：FTS5 trigram 的索引
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
比较内容（[pipeline.md](pipeline.md) §6.4），不是 O(增量)。
