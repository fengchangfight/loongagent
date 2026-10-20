# Context engineering — 分层可逆记忆

Language: zh

- **Up:** [design/context-ledger.md](../design/context-ledger.md)
- **Authority:** [ADR-0006](../decisions/0006-context-engine.md) · [ADR-0017](../decisions/0017-layered-context.md)
- **Code:** `crates/context/src/lib.rs`, `crates/kernel/src/ledger.rs`
- **Verify:** `cargo test -p loong-context -p loong-kernel` · `./scripts/check.sh`

本文是设计文档，不是决策记录。一旦方向定下，其中的决策点落入
ADR-0017（`docs/decisions/0017-layered-context.md`）。讨论中仍未定的问题见 [rollout.md](rollout.md) §14。

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
引用区间**。[recall.md](recall.md) §7.3（系统提示纪律）、[recall.md](recall.md) §7.2（`expand` 强制带原文）与 [failure-modes.md](failure-modes.md) §8（风险表）把这个纪律
做成机制，而不是靠约定。

---

## 1. 现状（改造的起点）

| 组件 | 位置 | 今天做什么 |
|---|---|---|
| 事件日志 | `crates/eventlog` | 每会话一个 append-only JSONL，`seq/id/parent_id/kind/time_ms/data`；**唯一真相**（ADR-0003） |
| 派生索引 | `crates/index` | SQLite `events(seq,id,parent_id,kind,ignorable,time_ms,data)`；可丢弃重算 |
| 上下文账本 | `crates/kernel/src/ledger.rs` | 每个进入模型的块是一条带 category/source/tokens/lifetime/pinned 的条目；**通往模型的唯一写路径**（ADR-0006） |
| 压缩 | `crates/kernel/src/turn/compaction.rs` | 越 `input_ceiling()`（预算 × `compact_at_percent`，或 模型窗口 − reserve）时：①清旧工具输出 ②丢弃最老的整轮对话 |
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

**产品后果**：理解层是"可选装"的那部分（[config.md](config.md) §11）；检索层**永远开启**，因为它是
"不丢原文"的兑现。用户截图里的判断分支（是否语义归纳）＝ 这里的理解层开关。

---

## 3. 两种运行模式

### R 模式 · Record-only（默认回滚路径，零依赖）

- L1 物化 + 关键词/实体索引 + 确定性 digest（[pipeline.md](pipeline.md) §6.2）全部工作，**不调用任何模型**。
- 前台携带"事实清单式"的 L4：最近任务列表、当前模块、状态、引用区间。
- 现有压缩**保留**：越过 ceiling 时清工具输出；只有**未装引擎**（`context_engine.is_none()`）时才丢最老交换，装了引擎的 R/S 模式都不丢消息。

### S 模式 · Semantic（可选装，开启后显著增强）

- 在 R 模式之上，L2 的"归纳部分"、L3、L4 由小模型生成。
- 与 R 模式相同，**不丢弃消息**（rollback 保留）：只清工具输出。
- 前台携带真正的 L4 认知地图，并暴露 `recall` 的 L2/L3 检索。

两种模式共用同一套存储、同一套引用语义、同一套 API；模式只改变**谁生成理解层**。


---

## 10. 与现有 ADR 的关系

| ADR | 关系 |
|---|---|
| 0003 JSONL 真相 + SQLite 派生 | **加强**：新增的是一套派生记忆，仍然可丢弃重建 |
| 0004 微内核、固定核心 | 引擎放**独立 crate**（拟 `crates/context`），内核只调用其只读接口；摘要不进入内核的决策逻辑 |
| 0006 上下文引擎 / 账本 | **修订 [principles.md](principles.md) §4**（压缩顺序与可逆性）；新增 `Category::CognitiveMap` 与 `Ledger::replace` |
| 0008 配置分层 | 新增 `[context_engine]` 与 `[summarizer]`：全局开关是 narrow-only |
| 0013 Rust 单二进制 | 小模型后端必须可选；默认构建不引入 C/C++ 工具链与网络下载 |
| 0015 两层树 | 引擎按会话内事件树工作；跨会话复用（T-10 知识而非会话）是**后续扩展**，见 [rollout.md](rollout.md) §12 |
| 0014 会话导出/导入 | 派生记忆不进 bundle（只导出 JSONL）；导入后按需重建，重建是纯派生操作。这正好检验"可丢弃可重建"这条硬要求 |

`loong-index`（`Index::search` 的 `LIKE` 路径）在 L1 索引就位后成为旧路径，
本设计不删除它，但新功能一律走新引擎。

---

## 章节去向

| 原章节 | 现在的位置 |
|---|---|
| §0 一句话、§1 现状、§2 目标结构、§3 两种运行模式、§10 与现有 ADR 的关系 | 本文件 |
| §4 原则（原文是唯一真相、上行收敛、不链式总结、只处理一次、先确定性后模型、稳态、前台优先） | [principles.md](principles.md) |
| §5 数据模型（位置、DDL、`atom.text`、实体抽取） | [data-model.md](data-model.md) |
| §6 流水线（统一形状、digest/synthesis、切段、上行触发、走一遍） | [pipeline.md](pipeline.md) |
| §9 事件种类（后台工作与摘要变更的可见事件） | [pipeline.md](pipeline.md) |
| §7 前台合成与 `recall`（唯一检索入口、系统提示纪律） | [recall.md](recall.md) |
| §8 失效模式与对策（核心风险表） | [failure-modes.md](failure-modes.md) |
| §11 配置、§11.0 `@` 文件引用、§11.2 模型不写文件 | [config.md](config.md) |
| §11.1 缺口清单与已落地（当前状态与限制） | [status.md](status.md) |
| §12 非目标、§13 分阶段实现、§14 待讨论 | [rollout.md](rollout.md) |
