# 有限工作内存，无限对话 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 上下文出问题时，任务降级而不是失败：供应商拒收不再抛错、步数用尽自动续、忘记的细节一跳查得回、前台长度不再单调增长。

**Architecture:** 四层改动，全部在现有结构里，不新建 crate。`providers` 增加一个错误分类谓词；`kernel/turn` 把"上下文超限"和"步数用尽"从终止条件改成降级条件，并新增存根层与任务卡；`context` 把 `recall` 的匹配从隐式 AND 改成 OR + 相关性排序，并在既有 `content` 尾部增加 `别名：` 一行；`ledger` 增加 `Stub` 类别与线索字段。JSONL 真相源、四层记忆结构、`semantic` 富化窗口都不变。

**Tech Stack:** Rust 2021；`cargo test -p loong-kernel -p loong-context`；`ScriptedModel`/`RecordingModel` 夹具；`loong-eventlog` 回放；`rusqlite`；`./scripts/check.sh`。

**Spec:** `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md`

## Global Constraints

- **绝不新增 schema 版本。** `Store::open` 在 `schema_version != SCHEMA_VERSION` 时返回 `StoreError::SchemaVersion`（`crates/context/src/store.rs:480`），而这是**打开失败**，不是迁移提示。所以别名写进 `content` 尾部，不改列。
- **降级路径不调用模型。** 阶梯每一格都是确定性操作。模型调用只允许出现在已有的空闲富化 worker 里。
- **不改停滞守卫。** `STALL_REPEAT_CALLS`/`STALL_FAILURES`/`STALL_SCRATCH_WRITES` 与 `compaction_thrash()` 的阈值一个都不动；它们防的是死循环，不是长对话。
- **`max_steps == 0` 仍然表示不限步数**，新的续期算术必须保持这一点且不溢出。
- **pinned 条目永不被淘汰。** 任何常驻内容都必须自带上限与溢出规则，否则它只会把别的条目挤出去。
- **契约：** 每个新 `fn`/`struct` 带 `Purpose:` + `Serves`/`Design`/`Up`/`Down`/`Verify`；测试函数只带一行 `Purpose`；触及文件时同步文件契约。
- **每个任务结束：** `cargo test -p loong-kernel -p loong-context` 绿；最后一次提交前 `./scripts/check.sh` 绿。
- **语言：** 代码注释与提交信息用英文（`fix:`/`feat:`/`test:`）；文档用中文，与同目录既有文档一致。

## Review Focus

- **不许把"降级"做成"静默降质"**：每一格都要在日志里留事件、在状态里可见。
- **不许让续期掩盖死循环**：续期只在"本段有进展"时发生，且次数有上限。
- **不许让 `recall` 的结果越长越难压**：检索结果必须落在可移出的那一档。
- **不许破坏前缀缓存**：任何重写都只在文本真的变了时发生（`seat_map` 的既有做法）。
- 旧会话必须照常打开：不升 schema、不改 JSONL 格式、不改事件载荷的既有字段含义。

---

## Task 1: 上下文超限不再让任务失败

现状：`crates/kernel/src/turn/loop.rs` 的模型调用是 `.map_err(KernelError::Model)?`，供应商返回 `context_length_exceeded` 时整轮报错退出，没有任何补救。

- [ ] **Step 1: 先写会失败的测试（providers）**

在 `crates/providers/src/lib.rs` 的测试模块加：

```rust
/// Purpose: 供应商用不同措辞报告上下文超限时都被识别为同一类信号。
#[test]
fn context_overflow_is_recognised_across_providers() {
    for text in [
        "HTTP 400 from https://api.x/v1: {\"error\":{\"code\":\"context_length_exceeded\"}}",
        "HTTP 400 from https://api.x/v1: prompt is too long: 210000 tokens",
        "HTTP 400 from https://api.x/v1: This model's maximum context length is 128000 tokens",
        "HTTP 413 from https://api.x/v1: request too large",
    ] {
        let e = ModelError::Transport(text.to_string());
        assert!(e.is_context_overflow(), "{text}");
    }
    let other = ModelError::Transport("HTTP 500 from https://api.x/v1: boom".to_string());
    assert!(!other.is_context_overflow(), "500 不是上下文问题，不许降级");
}
```

- [ ] **Step 2: 跑一次，确认失败**

`cargo test -p loong-providers context_overflow` → 编译失败（方法不存在）。这一步要留下输出。

- [ ] **Step 3: 实现谓词**

`crates/providers/src/lib.rs`，`impl ModelError`：

```rust
impl ModelError {
    /// Purpose: 判断这次失败是不是"上下文太长"这一类可降级的信号。
    /// Serves: `Kernel::run_inner`，把致命错误改成降级重试。
    /// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §5。
    /// Up: `Kernel::run_inner` 的错误分支。
    /// Down: 无调用；只做子串匹配。
    /// Verify: `cargo test -p loong-providers context_overflow_is_recognised_across_providers`。
    pub fn is_context_overflow(&self) -> bool {
        const NEEDLES: [&str; 6] = [
            "context_length_exceeded",
            "context length",
            "maximum context",
            "prompt is too long",
            "request too large",
            "too many tokens",
        ];
        let text = match self {
            ModelError::Transport(t) | ModelError::Protocol(t) => t,
        };
        let lower = text.to_lowercase();
        NEEDLES.iter().any(|n| lower.contains(n))
    }
}
```

注意 `HTTP 413`/"request too large" 也归入；`HTTP 500` 必须不命中。

- [ ] **Step 4: 先写会失败的测试（kernel）**

`crates/kernel/src/turn/tests.rs`：

```rust
/// Purpose: 供应商拒收上下文时，同一轮降级后仍然完成，而不是让任务崩掉。
#[test]
fn an_overflowing_request_is_compacted_and_retried_instead_of_failing() {
    // 模型第一次调用返回 context-length 错误，第二次起正常回答。
    // 修复前：submit 返回 Err，任务失败。
}
```

夹具：`ScriptedModel` 需要能"先报错再成功"。若现夹具不支持，加一个 `FailingOnceModel` 包装（只在本测试文件内），`complete` 第一次返回 `Err(ModelError::Transport("context_length_exceeded"))`，之后委托给内层。

- [ ] **Step 5: 实现降级分支**

`crates/kernel/src/turn/loop.rs` 里原来的：

```rust
.map_err(KernelError::Model)?
```

改成一份显式的"发一次请求"辅助函数，供降级阶梯复用：

```rust
/// Purpose: 发一次请求；上下文超限时按阶梯降级后重试，其他错误照旧上报。
/// Serves: `Kernel::run_inner` 的每一步，让上下文问题不再变成任务失败。
/// Design: `docs/superpowers/specs/2026-11-07-finite-memory-unbounded-conversation-design.md` §5。
/// Up: `Kernel::run_inner`。
/// Down: `Kernel::request_once`、`Kernel::compact_if_needed`、`Kernel::shed_to_minimal`。
/// Verify: `cargo test -p loong-kernel an_overflowing_request_is_compacted_and_retried_instead_of_failing`。
```

阶梯（每次失败只降一格，降完继续，最多 `MAX_OVERFLOW_RETRIES = 2` 格）：

1. 强制一次压缩，目标比常规更低：`compact_if_needed(Some(desperate_ceiling))`，`desperate_ceiling = before / 2`。
2. 仍被拒：`shed_to_minimal()` —— 保留 pinned 条目 + 最近一轮用户消息 + 最近一次助手回复，其余全部移出（存根由 Task 6 提供；本任务先用现有的 `clear_tool_outputs` + `drop_oldest_exchange` 循环到无法再减）。
3. 仍被拒：不再重试，改用**优雅结束**：返回 `Ok(TurnOutcome)`，`final_text` 说明"这一轮发不出去：上下文已满"，并写一条 `context.degraded` 事件。**不返回 `Err`。**

守卫：只有 `is_context_overflow()` 才走阶梯；网络错误、鉴权错误、500 仍按原样上报——那些不是上下文问题，掩盖它们等于掩盖故障。

- [ ] **Step 6: 跑测试**

`cargo test -p loong-kernel -p loong-providers` — 新测试绿，原有用例不回退。

- [ ] **Step 7: 补事件**

`crates/schema/protocol.toml` 增加：

```toml
[[events]]
name = "context.degraded"
payload = "ContextDegraded"
ignorable = true
```

`[[types]] name = "ContextDegraded"`，字段：`ladder`（string，命中的格名，如 `overflow-retry`/`minimal`/`give-up`）、`attempts`（u64）、`tokens_before`（u64）、`tokens_after`（u64）、`note`（string）。`./scripts/gen-schema.py` 重新生成；`--check` 必须绿。

- [ ] **Step 8: 提交**

`fix: degrade instead of failing when the provider rejects an oversized request`

---

## Task 2: 步数用尽自动续，不要人敲 continue

现状：`run_inner` 里 `let remaining = self.config.max_steps.saturating_sub(start);`，用完就返回 note。

- [ ] **Step 1: 先写会失败的测试**

`crates/kernel/src/turn/tests.rs`：

```rust
/// Purpose: 步数预算用尽后有进展就自动续期，人不需要再发一句 continue。
#[test]
fn a_turn_with_progress_continues_past_the_step_budget() {
    // max_steps = 2，脚本安排 5 个成功的工具调用后收尾。
    // 修复前：steps == 2，final_text 是 "reply to continue"。
    // 修复后：steps == 5，final_text 是模型的收尾语。
}

/// Purpose: 本段没有进展时续期不发生，死循环仍然会停。
#[test]
fn a_turn_without_progress_still_stops_at_the_budget() {
    // 脚本：每一步都调用同一个工具且结果相同（复用停滞守卫的重复判据）。
    // 期望：不续期，停止并给出原因。
}
```

- [ ] **Step 2: 确认失败**，`cargo test -p loong-kernel a_turn_with_progress_continues_past_the_step_budget`。

- [ ] **Step 3: 实现**

`KernelConfig` 加字段（`crates/kernel/src/turn.rs`，带 `Purpose`/`Serves`/`Design`/`Up`/`Down`/`Verify`）：

```rust
/// 一段步数预算用尽、且本段有进展时，最多自动续几段。0 表示不续（旧行为）。
pub max_continuations: usize,
```

默认值 `DEFAULT_MAX_CONTINUATIONS = 3`；`max_steps == 0`（不限）时该字段无意义，保持现有语义。

`run_inner` 的循环改成外层套一层"段"：

```rust
let mut segments = 0;
loop {
    let start = self.steps_taken;
    let remaining = self.config.max_steps.saturating_sub(start);
    let progressed_before = self.progress_marker();
    // …原有 for offset in 1..=remaining { … }
    // 段末：
    if self.config.max_steps == 0 { break; }
    if self.progress_marker() == progressed_before { break; }   // 无进展：不续
    if segments >= self.config.max_continuations { break; }      // 续到上限：停
    segments += 1;
    // 内部提示，不发给用户：把已完成的事收进任务卡（Task 7）或追加一条 system 便签
    self.append_continuation_note()?;
}
```

`progress_marker()`：本段成功完成的工具结果条数 + 写入的文件数（复用停滞守卫已经在记的计数，不新造一套）。

- [ ] **Step 4: 用户可见的停止语**

到上限才停，且文案要诚实：说明"连续 N 段没有实质进展"或"达到了续期上限"，并给出继续的具体办法。旧的 `"reply to continue"` 只在 `max_continuations = 0` 时保留。

- [ ] **Step 5: 两侧都要有测试**（续与不续），然后 `cargo test -p loong-kernel`。

- [ ] **Step 6: 提交** `feat: continue a productive turn past its step budget`

---

## Task 3: `recall` 一跳取回原文片段

现状：`recall` 只返回摘要 id + 摘要片段，要看原文得再 `expand` 一次（`crates/kernel/src/turn/recall.rs`）。

- [ ] **Step 1: 先写会失败的测试**

`crates/context/tests/retrieval.rs`（或 `crates/kernel/src/turn/tests.rs`，取决于现有 recall 测试位置）：

```rust
/// Purpose: 一次 recall 调用就带回命中处的原文片段，不再需要第二次 expand。
#[test]
fn recall_returns_an_evidence_excerpt_in_one_call() {
    // 写一条含 "配置层叠失效" 的事件，recall 该词。
    // 期望：返回文本里直接出现原文片段（含该词的上下文若干字符），
    // 并且仍带 <seq 区间> 以便需要全文时 expand。
}
```

- [ ] **Step 2: 确认失败**。

- [ ] **Step 3: 实现**

`RecallArgs` 增加 `excerpt_chars`（默认 `DEFAULT_EXCERPT_CHARS = 240`，上限 `MAX_EXCERPT_CHARS = 800`）。命中后，对每个命中摘要的引用区间取**最近命中事件**的原文，按 `excerpt_chars` 截断并在两侧加 `…`，附上 `seq`。

硬约束：单次 `recall` 的总输出字符数封顶（`MAX_RECALL_CHARS = 4_000`），超出时减少条数而不是截断每条——便宜的调用不能因为输出长而变贵。

- [ ] **Step 4: 失败的诚实回答**

零命中时返回 `"没有匹配到：<查询词>。可以换一个说法再试，或从 ep 区间直接 expand。"`——不编造、不静默返回空字符串。

- [ ] **Step 5: 检索结果可被再次移出**

确认 `recall` 的结果走 `ChatMessage::Tool`（现在已经是），因此会进入历史并可被 `clear_tool_outputs` 命中（`source` 以 `tool:` 开头）。补一条测试：

```rust
/// Purpose: 检索结果被清掉之后仍然查得回来，闭环才成立。
#[test]
fn a_cleared_recall_result_can_be_recalled_again() { /* 清 → 再 recall → 仍命中 */ }
```

- [ ] **Step 6: 提交** `feat: recall returns an evidence excerpt in one call`

---

## Task 4: 换词也查得到——OR + 相关性排序

现状：`Store::search`（`crates/context/src/store.rs:898`）与 `recall_at`（`crates/context/src/summaries.rs`）都是每个 term 一个 `LIKE` 条件再 AND 起来，一个词不对就整条查询返回零；排序只有 `seq_to DESC`。

- [ ] **Step 1: 先写会失败的测试**

```rust
/// Purpose: 一个不存在的词不会把整条查询清零。
#[test]
fn one_unknown_term_does_not_zero_the_query() {
    // 库里有一条含"层叠"的记录；查询词 ["层叠", "根本不存在的词"]。
    // 修复前：零命中。修复后：命中该条。
}

/// Purpose: 命中词多的记录排在命中词少的记录前面。
#[test]
fn results_are_ranked_by_how_many_terms_they_match() { /* 两条记录，A 命中 2 词、B 命中 1 词，A 在前 */ }
```

- [ ] **Step 2: 确认失败**。

- [ ] **Step 3: 实现 OR + 打分**

`Store::search`：每个 term 生成一个 `(… LIKE ?n OR …)` 片段，片段之间用 `OR` 连接；再生成一个 `score` 表达式，每个命中的 term 计分：

```sql
( (CASE WHEN atom.text LIKE ?1 THEN 1 ELSE 0 END)
+ (CASE WHEN atom.kind LIKE ?1 THEN 1 ELSE 0 END)
+ (CASE WHEN IFNULL(atom.tool,'') LIKE ?1 THEN 1 ELSE 0 END) ) + … AS score
```

`WHERE (score) > 0`、`ORDER BY score DESC, atom.seq DESC`、`LIMIT ?`。`recall_at` 同法，字段为 `title`/`content`，`title` 命中的权重高于 `content`（title 是主题，content 是细节）。

`SearchQuery.terms` 的文档注释要改：现在是"每个词都必须出现（隐式 AND）"，改成"任一词命中即返回，按命中数排序"——注释与行为不一致是本仓库明确禁止的。

- [ ] **Step 4: 一条必须写进测试的边界**

`terms` 为空时不许变成 `WHERE 1=1` 全表扫描：保持"空查询返回空结果"。

- [ ] **Step 5: 提交** `feat: rank recall results by match count instead of requiring every term`

---

## Task 5: 摘要写得更丰富——别名提高撞词概率

owner 的决定：**只做这一层，不做查询扩展，不引 embedding。**

- [ ] **Step 1: 先写会失败的测试**

```rust
/// Purpose: 富化产出的别名让不同说法也能命中同一条摘要。
#[test]
fn an_alias_of_the_summary_makes_a_different_wording_match() {
    // 摘要正文写"配置层叠失效"，综合（synthesis）给出别名"合并优先级、merge 顺序、overlay"。
    // 修复前：用 "overlay" recall 零命中。修复后：命中该摘要。
}

/// Purpose: 别名只用于命中，不出现在渲染给模型的摘要正文里。
#[test]
fn aliases_are_not_rendered_into_the_summary_text() { /* render 后不含 "别名：" */ }
```

- [ ] **Step 2: 确认失败**。

- [ ] **Step 3: 让综合（Synthesis）产出别名**

`crates/context/src/summarizer.rs`：`Synthesis` 加字段 `aliases: Vec<String>`（`#[serde(default)]`，缺省空，保证旧模型输出仍可解析）。富化提示词增加一段：

```
另外给出 3-6 个别名：同一件事的其它说法、英文叫法、涉及的模块或工具名、用户当时用的词。
只给词或短语，不要句子。
```

- [ ] **Step 4: 落库位置——不加列**

`crates/context/src/digest.rs` 的 `merge_synthesis` 末尾追加一行（与 `被否决：`/`原因：`/`未决问题：` 同一约定）：

```rust
if !synthesis.aliases.is_empty() {
    out.push_str(&format!("别名：{}\n", synthesis.aliases.join("；")));
}
```

理由写进代码注释：加列要升 `SCHEMA_VERSION`，而版本不匹配是打开失败，旧会话会直接失去记忆——用一行文本换零迁移。

- [ ] **Step 5: 渲染时跳过该行**

`crates/kernel/src/turn/recall.rs` 渲染摘要正文时跳过以 `别名：` 开头的行（它是索引，不是记忆）。搜索仍然扫描整列 `content`，所以别名可命中。

- [ ] **Step 6: `record` 模式下的诚实**

`record` 模式不做富化，因此没有别名。测试要固定这个事实而不是假装它有：`record` 模式下只靠 Task 4 的 OR 排序。

- [ ] **Step 7: 提交** `feat: let enriched summaries carry aliases so a different wording matches`

---

## Task 6: 旧消息转存根——移出但留地址

现状：引擎开启时压缩只清工具输出，用户消息和助手正文**只增不减**，所以长会话注定顶到上限。

- [ ] **Step 1: 先写会失败的测试**

```rust
/// Purpose: 引擎开启时，旧轮次也被移出前台，但留下可查的地址。
#[test]
fn old_exchanges_become_stubs_with_an_address() {
    // 长会话，超过上限。
    // 期望：更早的轮次变成一行存根，含 "<ep-a-b>"、线索词、以及 "recall seq a-b"；
    //      原文在 JSONL 里仍完整；expand 该区间能取回原文。
}

/// Purpose: 存根层让前台长度在长会话里停止单调增长。
#[test]
fn the_foreground_stops_growing_once_stubs_take_over() {
    // 跑 50 轮、每轮中等大小；第 40 轮与第 50 轮的前台 token 数差值 < 10%。
}

/// Purpose: 被否决的方案即使被移出也留下痕迹——它不能再靠重跑命令再生。
#[test]
fn a_rejected_plan_survives_as_a_stub() { /* 助手提出被用户否决 → 存根里标明"被否决" */ }
```

- [ ] **Step 2: 确认失败**。

- [ ] **Step 3: 数据侧：`Stub` 类别**

`crates/kernel/src/ledger.rs`：`Category` 加 `Stub`。`Entry` 加 `clues: Vec<String>`（来源：用户原话里的词、工具名、改动过的路径、实体名）。`Category::Stub` 在淘汰顺序里排在 `History` 之后、`Rules` 之前——存根自己也能被并段（Task 6 Step 6）。

- [ ] **Step 4: 生成存根（确定性，不调模型）**

`crates/kernel/src/turn/compaction.rs` 新增 `stub_old_exchanges(&mut self, keep_rounds: usize)`：

- 只处理"最早的一批完整轮次"，`keep_rounds` 由 `keep_newest_tokens` 换算；
- 每轮生成一行，格式固定：

```
[ep-00000118-00000143] 修复 config 层叠失效（7 步，完成）
  改动: src/config/merge.rs, src/config/layer.rs | 验证: cargo test -p loong-config 通过
  线索: layer merge 优先级 环境变量 | 原文: recall seq 118-143
```

- 字段全部来自本期已有事件（`tool.call`/`tool.result`/`tool.denied`），**不推断、不总结**；做不到就省略那一行，不写猜测。
- 用 `self.messages` 里的整轮替换成一条 `ChatMessage::System`（或 `User`，取决于该位置原来的角色；整轮压缩后放在原来第一条的位置，保持角色顺序合法）。

- [ ] **Step 5: 在压缩阶梯里接上**

`compact_if_needed` 的顺序改成：L0 `clear_tool_outputs` → 仍超线则 L1 `stub_old_exchanges` → 仍超线则 L2 相邻存根并段。事件沿用 `compaction.applied`，`dropped_messages` 记移出的消息数，并在 `summary` 里写明 `stubbed N exchange(s)`。

- [ ] **Step 6: 并段**

`merge_adjacent_stubs()`：相邻存根合成一行，区间取并集，线索取去重并集（封顶 12 个词）。这条保证存根数量本身有界。

- [ ] **Step 7: 与 `may_drop` 的关系**

`drop_oldest_exchange`（引擎关闭时的 Phase 2）保持不变。引擎开启时走存根，**不丢原文**——这正是 ADR-0017 的承诺，测试 `with_the_engine_on_a_long_session_never_drops_an_exchange` 必须仍然绿。

- [ ] **Step 8: 提交** `feat: stub old exchanges instead of letting the foreground grow`

---

## Task 7: 任务卡——pinned，且只有意志

为什么只放意志：一句"不要引入新依赖"被压成摘要就永久消失，重推也推不出来——它是人的偏好，不是从原始资料能算出来的东西。推导结论则相反，可以重推。

- [ ] **Step 1: 先写会失败的测试**

```rust
/// Purpose: 任务卡在长会话里从不消失，即使所有工具输出都被清掉。
#[test]
fn the_task_card_survives_every_compaction_pass() { /* 多轮压缩后仍在前台 */ }

/// Purpose: 任务卡超出上限时被合并而不是把别的条目挤出去。
#[test]
fn an_overflowing_task_card_is_merged_not_evicted() { /* 超限后仍在，长度受控 */ }
```

- [ ] **Step 2: 确认失败**。

- [ ] **Step 3: 实现**

`crates/kernel/src/turn/task_card.rs`（新文件，带完整文件契约）：

- `TASK_CARD_MAX_TOKENS = 800`；
- 内容来源全部是**确定性事实**：用户最近一次明确的目标句、用户说过的约束（由 `/constraint` 或显式关键词标记，若没有该机制则本字段留空且不猜）、已完成/待办（来自工具结果成败）、当前分支与最近验证命令；
- 座位方式照抄 `seat_map`：一条 pinned `System` 消息，**只在文本真的变化时重写**（否则前缀缓存全失效）；
- 溢出规则：按优先级裁剪——先丢"已完成"明细，再丢"最近验证命令"，目标句与约束**永不裁剪**；仍超限则把约束压缩成引用（"见 recall seq a-b"）。

- [ ] **Step 4: 在续期时刷新**

Task 2 的 `append_continuation_note()` 落在这里：续期 = 把本段完成的事并入任务卡。这样"段"与"任务卡"互为因果：续期靠它保持目标清晰，它也只在续期时有变化（前缀缓存友好）。

- [ ] **Step 5: 提交** `feat: seat a pinned task card so goals and constraints cannot be compacted away`

---

## Task 8: 文档、可见性与最终校验

- [ ] **Step 1: 改写维护文档**（Authority 仍是 ADR-0017）

| 文档 | 改什么 |
|---|---|
| `docs/design/context-ledger.md` | 三档驻留、`Stub` 类别、任务卡、pinned 溢出规则 |
| `docs/design/flows/context-compaction.md` | 降级阶梯取代"清 + 丢"；超限重试进流程图 |
| `docs/design/kernel-loop.md` | 步数续期替代"reply to continue"；`context.degraded` 事件 |
| `docs/context/recall.md` | 一跳取回、OR + 打分、别名；关闭"expand 只用摘要 id"这条缺口 |
| `docs/context/data-model.md` | `content` 尾部 `别名：` 的约定与其理由（不升 schema） |
| `docs/context/status.md` | 缺口清单按实际完成情况更新 |

- [ ] **Step 2: 可见性**

状态行显示降级档位与原因（如 `ctx 92% ↓stub`、`ctx full ↓minimal`）。**不静默降质**：质量下降允许，隐瞒不允许。

- [ ] **Step 3: 端到端验收（对应 spec §10）**

```bash
cargo test -p loong-kernel -p loong-context -p loong-providers
./scripts/check.sh
```

一条 100 步、每步大输出的会话必须跑完且日志里没有 stop/failure 事件；供应商超限必须在那轮内恢复；一个错词不清零；别名可命中；检索结果被清后仍可再查；50 轮后前台长度不再增长；任务卡超限被合并。

- [ ] **Step 4: 更新 `README.md` 的 `<!-- test-counts: … -->`**，与最后一次全量运行一致。

- [ ] **Step 5: 提交** `docs: record the finite-memory plan in the owning documents`

---

## 顺序与理由

1 → 2 先修"任务会死"，这是 owner 的痛点本身；3 → 4 → 5 让"忘记"变得便宜，因为存根层要靠它兜底；6 → 7 才动前台形态。反过来做（先存根）会在重查还贵的时候把细节移走，等于先拆梯子再造梯子。

## 不做的事

- **不做查询扩展**（owner 决定：够用比 perfect 便宜）。
- **不引 embedding**；真同义（"超时" vs "卡住"）暂时靠模型换个词再试。
- **不做重要性评分**；唯一例外是"同一段被重查超过 N 次则提升为原样保留"，由实际使用证明，不由事先猜测。
- **不做子 agent 上下文隔离**；那是另一件事，与本计划无关。
- **不动停滞守卫阈值**。
