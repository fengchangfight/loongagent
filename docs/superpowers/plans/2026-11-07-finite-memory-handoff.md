# 交接：有限内存、无限对话（施工状态）

给接手的 agent 看的。设计理由在
[spec](2026-11-07-finite-memory-unbounded-conversation-design.md)，逐条步骤在
[plan](2026-11-07-finite-memory-unbounded-conversation.md)。本文只写**现在在哪、下一步做什么、哪里有坑**。

## 目标（一句话）

上下文出问题时任务要**降级**，不能**失败**：供应商拒收不抛错、步数用尽自动续、忘掉的细节一跳查得回、前台长度不随对话长度单调增长。质量可以掉，聊天不能断。

## owner 已经拍板，不要重新讨论

1. **不引 embedding，不做查询扩展。** 原话："把摘要写得更丰富、撞词概率高一点就够了，不需要 perfect，因为 perfect 的代价太 heavy。" 只做：OR + 相关性排序、摘要别名。
2. **忘记了自动重查，不要问用户**要不要查。
3. **降级路径不调用模型**（降级时刻是延迟最敏感的时刻）；模型调用只留在已有的空闲富化 worker 里。
4. **不动停滞守卫阈值**（`STALL_REPEAT_CALLS` / `STALL_FAILURES` / `STALL_SCRATCH_WRITES`），也**不删**降级/停车的既有检查。

## 当前状态

分支 `feat/finite-memory`（从 `main` 切出）。

| 提交 | 内容 |
|---|---|
| `cd617df` | 上一轮：压缩窗口按"上限留给历史的余量"收缩，标记不缩短就不清，刹停提示不再误导 |
| `138d4d4` | **Task 1 完成**：`complete_with_ladder` 把供应商拒收从致命错误改成降级重试；`context.degraded` 事件；`ModelError::is_context_overflow` 区分大小问题与真故障 |

**Task 2 进行中，工作区此刻是 RED（故意）：**
`crates/kernel/src/turn/tests.rs` 已写好 5 个新测试并引用 `config.max_continuations`，
而 `KernelConfig` 还没有这个字段，所以 `loong-kernel` 测试**编译失败**。这是 TDD 的红灯，
接手第一件事就是把它推到绿。新测试名：

- `a_turn_with_progress_continues_past_the_step_budget`
- `a_turn_without_progress_still_stops_at_the_budget`
- `a_spinning_model_stops_at_the_repeat_guard`
- `a_turn_that_may_not_renew_stops_at_the_step_limit`
- `the_renewals_are_capped`

未开始：Task 3–8。

## 环境坑（会白白烧掉半天）

1. **工作区里躺着另一个会话的未提交改动**，20 个文件（`crates/mcp/*`、`crates/cli/*`、`crates/tui-app/*`、`crates/http/src/lib.rs`、`crates/tools/src/lib.rs`、`Cargo.lock`、`crates/kernel/src/turn.rs` 等）加两个未跟踪文件（`crates/tools/src/deferred.rs`、`docs/design/mcp-client.md`）。**不要 stage、不要提交、不要编辑、不要"顺手修好"它们。** 提交时只 `git add` 自己的 hunk。
   - 尤其注意 `crates/kernel/src/turn.rs`：它已有别人的 3 行（`mcp_specs` 字段）。Task 2 要在这里加 `max_continuations`，所以提交前务必 `git diff crates/kernel/src/turn.rs` 逐 hunk 确认，只 stage 自己那段。
2. **无法用 git worktree 隔离。** 这个沙箱里仓库外的路径全部不可写（`/home/fengchang/test` EACCES、`/run/user/1000`、`/tmp` 都不可写），`git worktree add` 必失败。就在当前分支原地干活。
3. **跑测试必须指定可写的临时目录**，否则有一个测试必失败：
   ```bash
   XDG_RUNTIME_DIR=$PWD/.scratch/rt TMPDIR=$PWD/.scratch cargo test -p loong-kernel
   ```
   （`.scratch/` 已被 `.git/info/exclude` 忽略；失败的是
   `the_pep_gives_the_shell_a_writable_scratch_directory`，与本次改动无关。）
4. **`cargo clippy` 没装**，别在它上面耗时间。
5. **`cargo fmt --all -- --check` 会报别人文件**（MCP 半成品）。只对**自己碰过的文件**跑 `rustfmt --edition 2021 <file>`。
6. `./scripts/gen-schema.py --check` 必须绿；改了 `crates/schema/protocol.toml` 就重新生成（会连带改 `clients/ts/*`）。

## 剩余任务

### Task 2（进行中）：步数用尽自动续

改 `crates/kernel/src/turn.rs`（`KernelConfig` 加字段）与 `crates/kernel/src/turn/loop.rs`（`run_inner` 分段）。

- 加 `pub max_continuations: usize`，默认 `DEFAULT_MAX_CONTINUATIONS = 3`，`0` = 不续（旧行为）。字段和常量都要完整契约块。
- `run_inner` 的 `for offset in 1..=remaining` 外面套一层"段"循环：段末若 `max_steps == 0` 就结束；若本段 `progress_marker()` 没变就结束（无进展不续）；若段数到 `max_continuations` 就结束。
- `progress_marker()` 直接用已有计数，**不要新造一套**：本段成功工具结果数（`Kernel::note_tool_result` 已经在记 `consecutive_failures`，成功即进度更简单可靠）+ 写入的文件数。
- 到上限时的文案要诚实，说明"续了 N 段后停下"，并且**必须包含 `renewed` 这个词**（`the_renewals_are_capped` 在断言它）。

**两处既有测试会被续期撞坏，必须一并改，不能靠删断言让它变绿：**

| 测试 | 为什么坏 | 怎么改 |
|---|---|---|
| `the_step_limit_stops_a_spinning_model`（`tests.rs` 约 555 行） | 3 步都成功 → 触发续期 → 第 4 步被重复守卫拦下，文案是"stall guard"，不再含 "step limit" | 该测试的目的是"步数上限"，加 `config.max_continuations = 0` 保持单一目的 |
| `the_step_budget_survives_an_approval_resume`（约 689 行） | 同理，续期后 `out.steps` 会变成 4、文案不再是 "step limit" | 同样加 `config.max_continuations = 0`；它要证明的是"审批恢复不重复给预算"，与续期无关 |

验收：
```bash
XDG_RUNTIME_DIR=$PWD/.scratch/rt TMPDIR=$PWD/.scratch cargo test -p loong-kernel
```
6 个相关测试全绿，且 `loong-kernel` 整体不回退（Task 1 后是 139 passed / 1 ignored）。

### Task 3：`recall` 一跳取回原文片段

`crates/kernel/src/turn/recall.rs`：加 `excerpt_chars`（默认 240，上限 800），命中后直接带原文片段 + `seq` 区间；单次输出字符封顶 4000，超了**减条数**而不是截断每条。零命中要如实说"没匹配到：<查询词>"。补一个测试：检索结果被 `clear_tool_outputs` 清掉后**仍能再查出**（闭环，不能漏）。

### Task 4：换词查得到——OR + 相关性排序

`crates/context/src/store.rs`（`Store::search`，约 898 行）与 `crates/context/src/summaries.rs`（`recall_at`）：现在每个 term 一个 `LIKE` 再 AND，一个词不对就整条返回零；改成 OR + `score`（命中词数）排序，`title` 权重高于 `content`；同分按 `seq DESC`。`terms` 为空时**保持返回空结果**，不许退化成全表扫描。`SearchQuery.terms` 的文档注释必须同步改（现在写的是"每个词都必须出现"，注释与行为不一致是本仓库禁止的）。

### Task 5：摘要别名（owner 只批准了这一层）

`crates/context/src/summarizer.rs` 的 `Synthesis` 加 `aliases: Vec<String>`（`#[serde(default)]`，旧模型输出仍可解析），富化提示词要 3–6 个别名。落库**追加到 `content` 尾部一行** `别名：…`，**绝不加数据库列**——`Store::open` 在 schema 版本不匹配时是**打开失败**（`crates/context/src/store.rs:482`），加列会让所有旧会话丢掉记忆。渲染摘要正文时跳过以 `别名：` 开头的行（它是索引不是记忆），但搜索仍扫整列。

### Task 6：旧消息转存根（移出但留地址）

这是"前台不再变长"的关键：现在引擎开着时只清工具输出，用户消息和助手正文只增不减。`crates/kernel/src/ledger.rs` 加 `Category::Stub` 与 `Entry.clues`；`crates/kernel/src/turn/compaction.rs` 加 `stub_old_exchanges`（确定性抽提，**不调模型**）与 `merge_adjacent_stubs`。存根格式与字段要求见 plan。**必须保留** `with_the_engine_on_a_long_session_never_drops_an_exchange` 为绿（ADR-0017 的承诺）。

### Task 7：任务卡（pinned，只放意志）

新文件 `crates/kernel/src/turn/task_card.rs`：≤800 tokens，pinned，只放目标与你明说过的约束/否决，**不放推导结论**（结论可重推，偏好推不出来）。座位方式照抄 `seat_map`：**只在文本真的变化时重写**，否则前缀缓存全失效。溢出按优先级裁剪，目标与约束永不裁。

### Task 8：文档、可见性与收尾

改写 `docs/design/context-ledger.md`、`docs/design/flows/context-compaction.md`、`docs/design/kernel-loop.md`、`docs/context/recall.md`、`docs/context/data-model.md`、`docs/context/status.md`。状态行显示降级档位（不静默降质）。最后：`./scripts/check.sh` 绿、`README.md` 的 `<!-- test-counts: … -->` 与实际一致（当前那次全量是 904 passed / 11 ignored，加上 Task 2 的两个新测试会变，**以最后一次全量运行为准**）。

## 提交纪律

- 先 RED 再 GREEN，RED 的输出要留在提交信息或交接记录里，证明测试真的会失败。
- 一次提交一个任务；信息用 `fix:` / `feat:` / `test:` + 英文描述（与既有提交一致）。
- 每步只 `git add` 自己的 hunk。提交前跑 `python3 scripts/check-modules.py`（现在只剩 `crates/mcp/src/config.rs:394` 一条，是**别人的**，别去修）和 `python3 scripts/check-docs.py`。
- 契约：新 `fn`/`struct` 带 `Purpose:` + `Serves`/`Design`/`Up`/`Down`/`Verify`；测试函数只带一行 `Purpose`；`Verify:` 必须写一条本仓库真能跑的命令。

## 我不确定的地方（接手时请自己判断）

1. **`progress_marker` 的判据**：我倾向"本段有工具调用成功即算进展"，但这会把"一直在读同一个文件、读成功"当成进展——重复守卫会兜住，可 `read_file` 换着文件读也算进展，可能掩盖空转。若观察到该续不续/该停不停，先调这里。
2. **`keep_window` 的"余量一半"**是经验值，不是从数据推出来的；若仍频繁撞线，先调这个 1/2。
3. **存根线索词优先取用户原话**：如果你常换说法提同一件事，这条会退化——那正是 Task 4/5 要补的，所以真出现"存根查不回"，先看 4/5 的命中率而不是改 6 的取词规则。

## 一句话提醒

如果你自己的会话撞上 `compaction thrash` 或 `step limit` 停下，那不是你写错了，**那正是本计划要修的产品缺陷**。把每一步的改动做小、每完成一个任务就提交，就能绕开它。
