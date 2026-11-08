# loongagent reverse — 交接文档（给接手实现的 agent）

## 0. 一句话状态

设计与计划已定稿并提交在 `feature/reverse` 分支；**实现尚未开始**（Task 1 只做了侦察，一行代码没 copy）。你从 Task 1 开始执行。

## 1. 环境布局（先读这个）

| 项 | 位置 |
|---|---|
| 你的代码工作区 | `/home/fengchang/test/loongcoder/.worktrees/reverse`（git worktree，分支 `feature/reverse`，起点 main@1f54ef4，最新提交 74d7e1f） |
| 主工作区 | `/home/fengchang/test/loongcoder`（在 main 上，有一个与本工作**无关**的 stash `wip: tui-app changes`，别动） |
| 设计文档（权威） | worktree 内 `docs/design/reverse.md` |
| 实现计划（任务清单） | worktree 内 `docs/superpowers/plans/2026-10-24-loongagent-reverse.md` |
| 执行台账 | worktree 内 `.superpowers/sdd/2026-10-24-loongagent-reverse/progress.md` |
| 各任务 brief | 同上目录 `task-N-brief.md`（由 task-start 脚本生成） |
| 上游源码（只读参考） | `/home/fengchang/test/craft-anything`（MIT OR Apache-2.0） |

**纪律：只在 worktree（`.worktrees/reverse`）里写代码。** 主工作区不要碰。

## 2. 你要做什么（用户拍板的需求）

`loongagent reverse <target>` 一条命令：起本地 Web 服务、自动开浏览器、CLI 打印 URL。页面对本地二进制 / 网站 URL / APK 做逆向：左侧 `.reverse/` 文件树+预览，右侧流式聊天，顶栏 Study/Build 两个按钮。Study 产出 markdown 到 `.reverse/study/`；Build 等用户点击才开始，产出到 `.reverse/build/`，之后多轮聊天迭代。约 50 个核心逆向工具从 craft-anything copy 进仓库，经 stdio MCP server 挂给 session。**不做** Check/Ready、JEB/apktool/adb 动态链、iOS/游戏 target。

## 3. 已锁定的关键决策（不要推翻，有异议先问用户）

1. **工具 copy 源码，不引入 git/cargo 依赖**。包名统一 `loong-reverse-*`，放 `crates/reverse/vendor/`。
2. **只 copy 约 50 个核心工具**，不是 320 个。保留清单在设计文档 §1：core 11（调查记忆）+ native 16（二进制静态分析）+ web/js 11（网站）+ android 8（APK 静态）+ 辅助 3。
3. **隔离**：逆向工具只存在于 reverse 会话。manifest 写独立的 `.reverse/mcp.toml`（+ `.reverse/mcp.trusted`），**绝不碰**项目自己的 `.loongagent/mcp.toml`（trust 是对文件全字节 hash，碰了会把项目已 trust 的 server 全部打失效）。reverse 退出删除自己这对文件。
4. **trust 例外**：程序自写的 `.reverse/mcp.toml` 自动 trust，要在 `docs/design/security-trust.md` 补一节说明（Task 7）。
5. **UI 单文件 index.html**，无构建步骤、无 CDN，模仿 craft-anything 桌面布局的简化三栏。
6. **模型/会话/权限全复用** loongagent 现有机制；web 服务复用 `loong_web::{random_token, bind}` + `cli/web.rs` 的 WebSession 桥。
7. `.reverse/` 用 `.gitignore: *` 整个不 track。

## 4. 侦察结论（已核实，直接用，别重复查）

**Workspace 与工具链差异（Task 1 的关键坑）**：
- craft-anything：edition **2024**、rust-version 1.97、resolver "3"；loongcoder：edition **2021**、rust-version 1.80、resolver "2"。copy 后先按原样 build，若 2021 不兼容再统一升 workspace（用户环境 rustup 有 stable，编译器本身没问题）——这是一个待你裁定并记 ledger 的点。
- 所有 vendor crate 的 `Cargo.toml` 用 `xxx.workspace = true` 继承，且 `[lints] workspace = true`（上游根 lints：`unsafe_code = "forbid"`、`dbg_macro/todo = "warn"`）。loongcoder 没有 `[workspace.lints]`。处理方案（推荐）：把各 crate 的 `*.workspace = true` 展开成显式值，删 `[lints] workspace = true` 改为 crate 内显式 `[lints.rust] unsafe_code = "forbid"`（上游禁 unsafe，别放松）。
- workspace 依赖版本（copy 时需要的版本号都在 craft-anything 根 `Cargo.toml` 的 `[workspace.dependencies]`）：serde 1、serde_json 1、serde_path_to_error 0.1、sha2 0.10、hex 0.4、bitflags 2(serde)、schemars 1、log 0.4、toml 0.9、thiserror 2、miniz_oxide 0.8、object 0.36、iced-x86 1.21、yaxpeax-arm 0.5、yaxpeax-arch 0.3、rustc-demangle 0.1、wasmparser/wasmprinter 0.261、regex 1、oxc_* 0.153（js crate 用）。
- engine 额外依赖：dirs 6、base64 0.22、png 0.17、image 0.25（jpeg/webp/gif，截图用）。`wasm-plugins` feature（wasmtime）默认关——**整个删掉**，连同 `craft-anything-engines` 依赖。
- platform 的重依赖（process-wrap、portable-pty、vt100、xcap、nix、windows）：裁 runtime/PT Y 工具后大部分可删，以编译为准。

**接口已核实存在**（在 craft-anything 源码里）：`Engine`、`Registry::with_all_tools()`、`serve_stdio(engine: Arc<Engine>, opts: ServerOptions)`（`crates/mcp/src/lib.rs`）。copy 后名字不变，只改 crate 名前缀。

**loongcoder 侧已核实的复用点**：`loong_web::{random_token, bind, serve}`（`crates/web/src/lib.rs`）；`McpSet::load(workspace)`/`trust_now`（`crates/mcp/src/config.rs`）；`cli/web.rs` 的 WebSession 桥（ChildTransport + cwd 填充）。注意 `McpSet::load` 目前只读 `<workspace>/.loongagent/mcp.toml`——Task 2 要扩展它合并 `.reverse/mcp.toml`。

**license**：craft-anything 根有 `LICENSE-APACHE` 和 `LICENSE-MIT`，Task 1 Step 4 要把全文放进 `crates/reverse/vendor/ATTRIBUTION.md`。

**APK 测试资源**：`craft-anything/crates/formats/tests/` 有现成 dex/android 解析测试（`dex_*`、`android_string_pool_bounds`），copy 时带过来。

## 5. 执行流程（按 superpowers 规则）

1. 加载 `executing-plans`（本会话无 subagent 派发工具，上一任已用内联执行；你若也没有派发工具就同样处理，最终整分支 self-review 并在总结里明示局限）。
2. 加载 `test-driven-development`，每个任务 RED→GREEN。
3. 每个任务：`scripts/task-start <plan> N` 取 brief 和 BASE → 按 step 执行 → 每步对 `Expected:` 核对真实输出 → `scripts/task-done <plan> N BASE -- <test命令>` 落 ledger。**每步命令输出要真看**。
4. 台账规则：每个偏离 brief 的决定写 `Task N: Ruling: ...` 行到 progress.md；ledger 是压缩后的记忆，勤写。
5. 7 个任务完成后：跑 `review-package` 做整分支 review，修 Critical/Important（各带 RED→GREEN 测试），Minor 记 ledger。
6. 收尾用 `finishing-a-development-branch`；提交前 `./scripts/check.sh` 必须全绿（含 `check-modules.py` 的契约头检查——**每个新 .rs 文件都要 `//!` 模块契约**，格式照 `crates/cli/src/web.rs` 头部，Design 指向 `docs/design/reverse.md`）。

## 6. 已知待裁定项（执行时记 ledger）

- **edition 差异**：先按原样 build，2021 编不过再升（见 §4 第一条）。
- **Task 2 的加载语义**：上一任已裁定「`McpSet::load` 存在即合并 `.reverse/mcp.toml`，普通会话也读，隔离靠退出删除」（ledger Pre-flight 有记录）。实现照此，不必重议。
- **vendor 契约头**：上游代码没有 loongcoder 的契约头体系，Step 4 要批量补；上游原有的 doc comment 保留，别删。

## 7. 验收（设计文档「验收方式」全 8 条要过）

重点提醒三条最易漏的：⑤ `loongagent reverse app.apk` 走通；⑦ 项目已有 `.loongagent/mcp.toml` 且已 trust 时 reverse 进出的场景；⑥ `git status` 不显示 `.reverse/` 内容。完整清单见设计文档末尾。

## 8. 上一任留下的进度

- `feature/reverse` 分支唯一提交：74d7e1f（设计文档 + 计划）。
- Task 1 brief 已生成（`task-1-brief.md`，BASE 74d7e1f）。直接 `task-start ... 1` 开干。
