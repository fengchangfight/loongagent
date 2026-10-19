# 配置与模型行为

Language: zh

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0006](../decisions/0006-context-engine.md) · [ADR-0017](../decisions/0017-layered-context.md) · [ADR-0008](../decisions/0008-configuration-layering.md)
- **Code:** `crates/config/src/layer.rs`, `crates/tui-app/src/mention.rs`, `crates/kernel/src/turn/loop.rs`
- **Verify:** `cargo test -p loong-config -p loong-context -p loong-kernel` · `./scripts/check.sh`

## 11. 配置

**owner 决定（2026-10-13）：小模型后端先用本机 Ollama。** 默认模型已按 2026-10-14 的
忠实度重测改为 `qwen2.5:7b-instruct`（见 [rollout.md](rollout.md) §14 未决项 2）。
**真正被解析的键**（`crates/config/src/layer.rs`，2026-10-14 核对）：只有下面这些。
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
（常量）。

`window_calls` / `window_tokens` 是**每个空闲窗口**的硬预算（[principles.md](principles.md) §4.6），不是"每个步边界"，
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
F2 面板                            # 已实现：账本分类 + 记忆状态

loong context build [--all]        # **草案，未实现**
loong context explain              # **草案，未实现**（L4 全文只出现在请求里）
/context                            # **草案，未实现**（TUI 用 F2）
```

`local semantic` 写在 `~/.loongagent/local.toml`（`local add` 的同一个文件），因此只对
`loongagent tui --local` 生效；它用与 `local add` 相同的"文本手术"而不是重新序列化，
文件里的注释和其他键不会被抹掉。摘要 provider 目前只有 `ollama` —— 见 [rollout.md](rollout.md) §14 未决项 1 的
实测（OpenAI 兼容的 `/v1` 忽略 `think:false`），写别的 provider 会被具名拒绝。

（`loong context status` 里的 L4 计数一度用 `summary_count(4)`，那恒为 0；已改为与
`memory_layers` 同一口径 —— L4 不是存储的行，见 [data-model.md](data-model.md) §5.2。）

`ContextExplain` 增加（**已实现，2026-10-14**）：`memory_mode`（引擎关时为空，否则
`record`/`semantic`；"开/关"只认它，不认账本里有没有 `cognitive_map`）、
`memory_summarizer`、`memory_degraded`（降级原因，空=健康）、
`memory_layers[]`（L1–L4 的条数与 token；L4 是"有地图=1 / 还没有=0"）、
`memory_worker`（state / windows / model_calls / enriched / failed / pending /
deferrals / last_error）。前台"为什么这句话在这里"的能力保持。
TUI 在 F2 面板渲染它们，并在降级时于状态栏常驻 `mem ⚠ degraded`。

---

## 11.0 TUI：`@` 文件引用

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

## 11.1 模型不写文件、只在聊天里贴代码


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
   `tool_calls` delta、`content` 为空。这是 §11.1 开头那条结论的又一例 —— **不稳定**。

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
