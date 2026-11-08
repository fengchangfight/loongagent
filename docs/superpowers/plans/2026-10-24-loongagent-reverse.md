# loongagent reverse（最小版逆向工程模式）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `loongagent reverse <target>` 起本地 Web 界面，对本地二进制、网站 URL 或 APK 文件做 Study（产出 markdown 文档到 `.reverse/study/`）和用户手动触发的 Build（产出到 `.reverse/build/`），逆向工具以 stdio MCP server 提供。

**Architecture:** 从 craft-anything copy 约 50 个核心逆向工具（调查记忆 + 二进制静态分析 + 网站检查 + APK 静态分析）到 `crates/reverse/vendor/`，由 `loong-reverse-mcp` 二进制以 stdio MCP 暴露；`loongagent reverse` 把 manifest 写进独立的 `.reverse/mcp.toml`（程序自 trust），复用 loong-web 的 token+WS 桥起服务并打开浏览器；会话就是普通 loongagent session。

**Tech Stack:** Rust workspace（`members = ["crates/*"]`）、单文件无构建 HTML 页面、tungstenite WebSocket（loong-web 已有）、craft-anything 的 ToolSpec/Engine 契约。

**Spec:** `docs/design/reverse.md`（执行者必须一并阅读；本文档只固定实现决策，意图与取舍看 spec）

## Global Constraints

- 逆向工具**只允许出现在 reverse 会话**：manifest 只写 `.reverse/mcp.toml`，普通会话不可见；退出删除（spec「生命周期与隔离」）。
- **不引入对 craft-anything 的 git/cargo 依赖**：工具代码 copy 进 `crates/reverse/vendor/`，包名 `loong-reverse-*`。
- 项目的 `.loongagent/mcp.toml` **一个字节不碰**；trust 是对文件全字节 hash。
- **不做** Check/Ready；target 支持本地二进制文件、网站 URL、APK 静态分析（不做 JEB/apktool/adb 动态链）。
- 每个新 `.rs` 文件带 `//!` 模块契约（`Design: docs/design/reverse.md`），`pub` 及非平凡 item 带 `Purpose:`/`Serves:`/`Design:`/`Up:`/`Down:`/`Verify:` 五标签（测试函数一行 `Purpose:`）；契约格式照 `crates/cli/src/web.rs` 头部。
- 页面单文件、无构建步骤、无 CDN；bind 127.0.0.1 + 随机端口 + token。
- 每个 commit 前 `cargo build` 绿；收工前 `./scripts/check.sh` 全绿。
- copy 的 vendor crate 在 `crates/reverse/vendor/ATTRIBUTION.md` 注明来源 commit 与 MIT/Apache-2.0 双 license 全文。

## Review Focus

1. **损坏/超大/非二进制的 target 文件**（比如传一个目录或 10GB 文件）：`core_target_open`/`core_target_triage` 应返回结构化错误而非 panic，聊天里可读。测试归 Task 4。
2. **无法访问的网站**（DNS 失败、超时、需要登录）：`web_site_open` 返回带原因的错误，会话不死。测试归 Task 4。
3. **本机没有可用浏览器/CDP**：web 工具返回明确 availability 错误，二进制分析路径不受影响。测试归 Task 4。
4. **上次 reverse 异常退出（kill -9）残留 `.reverse/mcp.toml`**：下次启动覆盖重写而非报错或叠加。测试归 Task 2。
5. **同目录已有 `.loongagent/mcp.toml` 且其中有名为 `loong-reverse` 的 server**：启动报错提示改名，不静默覆盖。测试归 Task 2。
6. **target 路径不存在或不可读**：CLI 在起服务前就报错退出，不开浏览器。测试归 Task 5。
7. **损坏或非标准的 APK**（比如伪装的 zip、无 classes.dex）：`android_package_show` 返回结构化错误说明缺什么，不 panic。测试归 Task 4。

---

### Task 1: vendor 工具的 copy 与裁剪

**Files:**
- Create: `crates/reverse/vendor/{evidence,platform,formats,disasm,js,analysis,engine,mcp}/`（从 craft-anything 对应 crate copy）
- Create: `crates/reverse/vendor/ATTRIBUTION.md`
- Create: `crates/reverse/README.md`（来源 commit、裁剪清单、同步方法）
- Modify: `Cargo.toml`（workspace members 已含 `crates/*`，但 `crates/reverse/vendor/*` 是嵌套，需要确认被 glob 覆盖或在根 `Cargo.toml` 显式列出）

**Interfaces:**
- Consumes: craft-anything 源码（/home/fengchang/test/craft-anything，MIT/Apache-2.0）。
- Produces: 包 `loong-reverse-evidence/platform/formats/disasm/js/analysis/engine/mcp`；`loong_reverse_engine::Engine`、`loong_reverse_engine::Registry::with_all_tools()`（裁剪后只含保留工具）；`loong_reverse_mcp::serve_stdio(engine: Arc<Engine>, opts: ServerOptions)`。

- [ ] **Step 1: copy 八个 crate 并改包名**

从 craft-anything 的 `crates/{evidence,platform,formats,disasm,js,analysis,engine,mcp}` 整目录 copy 到 `crates/reverse/vendor/`，`Cargo.toml` 的 `name` 改为 `loong-reverse-*`，内部 `path` 依赖同步改名；删除 workspace 继承（`workspace = true`）的字段改为显式值。在 `crates/reverse/README.md` 记录来源仓库路径与 commit hash（`git -C /home/fengchang/test/craft-anything rev-parse HEAD`）。

- [ ] **Step 2: 裁剪 engine 的工具注册表**

打开 copy 后的 `engine`，把工具注册缩减到 spec「保留清单」的约 50 个 id（core 11 + native 16 + web/js 11 + android 8 + 辅助 3，具体 id 见 spec）。删除被砍工具的源文件、`Registry` 里的注册行、以及只被它们使用的 `platform`/`analysis` 模块（PTY/进程注入、check 比对）。删除 `crates/engines` 依赖（Ghidra/adb/frida 桥）及其调用点。删 `Cargo.toml` 里随之不再需要的依赖。

- [ ] **Step 3: 编译并跑 vendor 自带测试**

Run: `cargo build -p loong-reverse-engine -p loong-reverse-mcp && cargo test -p loong-reverse-engine`
Expected: 编译绿；copy 过来的单测绿（裁剪中删掉的测试除外）。`Registry::with_all_tools().all().len()` 应 ≈ 50，写一个小测试钉住（id 清单比对，防止回潮）。

- [ ] **Step 4: 补模块契约头与 ATTRIBUTION**

给每个 vendor crate 的每个 `.rs` 文件补 `//!` 契约头（`Design: docs/design/reverse.md`，`Verify: cargo test -p loong-reverse-<name>`）；item 级 doc 已有的保留、缺的补最简五标签。可写一次性脚本批量插入文件头再手工补 item。写 `ATTRIBUTION.md`（来源 + MIT/Apache-2.0 全文）。

Run: `./scripts/check-modules.py --paths $(cd crates/reverse/vendor && git ls-files '*.rs' | sed 's|^|crates/reverse/vendor/|' | tr '\n' ',')`
Expected: 退出码 0。

- [ ] **Step 5: Commit**

```bash
git add crates/reverse Cargo.toml
git commit -m "feat(reverse): vendor craft-anything core reverse-engineering tools"
```

### Task 2: `loong-mcp` 第二 manifest 来源（`.reverse/mcp.toml`）

**Files:**
- Modify: `crates/mcp/src/config.rs`（`McpSet::load`、`MCP_DIR` 常量附近、`trusted` 校验）
- Test: `crates/mcp/src/config.rs` 的 `#[cfg(test)]`

**Interfaces:**
- Consumes: 现有 `McpSet::load(workspace)`（只读 `<workspace>/.loongagent/mcp.toml`）、`trust_now`。
- Produces: `McpSet::load` 变为同时合并可选的 `<workspace>/.reverse/mcp.toml`（trust 记录为 `<workspace>/.reverse/mcp.trusted`，与主 manifest 各自的 hash 独立校验）；`McpServer` 结构新增 `origin: ManifestOrigin`（`Workspace | Reverse`）字段；`mcp list` 输出标出来源。行为规则：`.reverse/mcp.toml` 存在即加载（普通会话也读——靠 reverse 退出删除保证隔离，spec 已写明取改动小的方案）。

- [ ] **Step 1: 写失败测试**

在 `config.rs` 测试中新增（tmp 目录搭 `.loongagent/mcp.toml` 与 `.reverse/mcp.toml` + 各自 `mcp.trusted`）：

```rust
#[test]
fn reverse_manifest_merges_alongside_workspace_manifest() {
    // 两个来源各一个 server，load 后两个都在，origin 正确
}

#[test]
fn reverse_manifest_untrusted_alone_fails_closed() {
    // 主 manifest trusted、.reverse 未 trust → 仅 .reverse 的 server 被摘出到 trust_failure，主 server 正常
}

#[test]
fn workspace_manifest_hash_untouched_by_reverse_pair() {
    // 写/删 .reverse 一对文件前后，主 manifest 的 trusted 校验结果不变
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p loong-mcp reverse_manifest`
Expected: FAIL（当前 load 不读 `.reverse/`）。

- [ ] **Step 3: 实现合并加载**

`McpSet::load` 在读主 manifest 后尝试读 `.reverse/mcp.toml`：不存在则跳过；存在则按同样的 parse + trust 校验流程处理（trusted 指纹文件为 `.reverse/mcp.trusted`），把 server 并入集合，`origin` 标记为 `Reverse`。主 manifest 的 parse/trust 逻辑一行不改。`mcp.rs` 的 `list` 输出加来源列。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p loong-mcp`
Expected: 全绿（含旧测试）。

- [ ] **Step 5: Commit**

```bash
git add crates/mcp
git commit -m "feat(mcp): merge optional .reverse/mcp.toml as a second manifest source"
```

### Task 3: `loong-reverse-mcp` stdio server 二进制

**Files:**
- Create: `crates/reverse/mcp-server/Cargo.toml`、`crates/reverse/mcp-server/src/main.rs`
- Test: `crates/reverse/mcp-server/tests/stdio.rs`

**Interfaces:**
- Consumes: `loong_reverse_engine::{Engine, Registry}`、`loong_reverse_mcp::serve_stdio`（Task 1）。
- Produces: 二进制 `loong-reverse-mcp`；argv: `loong-reverse-mcp [--workspace <dir>]`（target 不预置，由首个 `core_target_open` 传入）；stdio 上说 MCP JSON-RPC。

- [ ] **Step 1: 写失败测试**

`tests/stdio.rs`：spawn 二进制，stdin 写入一条 MCP `initialize` 请求行，断言 stdout 在超时内返回合法 JSON-RPC 响应且 `tools/list`（若 initialize 后请求）包含 `core_target_triage`、`native_function_pseudocode`、`web_site_open`、`android_package_show`，不包含 `native_cursor_goto`、`android_jeb_decompile`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p loong-reverse-mcp`
Expected: FAIL（二进制不存在）。

- [ ] **Step 3: 实现 main**

约 40 行：解析 argv，构造 `Registry::with_all_tools()` 与 `Engine`，调 `serve_stdio`。全部输出走 stdout 的 JSON-RPC，日志走 stderr（MCP 约定）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p loong-reverse-mcp`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/reverse/mcp-server
git commit -m "feat(reverse): loong-reverse-mcp stdio server binary"
```

### Task 4: vendor 工具的健壮性（Review Focus 1-3）

**Files:**
- Modify: `crates/reverse/vendor/engine/src/`（core_target_open/triage、web 工具的调用处）
- Test: `crates/reverse/vendor/engine/src/` 既有测试模块

**Interfaces:**
- Consumes: Task 1 的 Engine。
- Produces: 错误以 `Evidence`/工具错误 JSON 返回，不 panic；web 工具无浏览器时返回 `availability` 类错误。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn open_directory_as_target_returns_error_not_panic() { /* core_target_open 传 tmp 目录 */ }

#[test]
fn triage_random_bytes_returns_unknown_kind() { /* 写 1KB 随机字节文件做 triage */ }

#[test]
fn web_site_open_unreachable_host_returns_error() { /* http://127.0.0.1:1/ */ }

#[test]
fn android_package_show_on_zip_without_dex_returns_error() { /* 造一个无 classes.dex 的 zip 改名 .apk */ }

#[test]
fn android_package_show_on_real_fixture_apk() { /* 用测试夹具最小 APK（可复制 craft-anything 的测试资源或现场构造最小 dex）断言包名可读出 */ }
```

- [ ] **Step 2: 跑测试确认现状**

Run: `cargo test -p loong-reverse-engine`
Expected: 列出哪些 panic/挂起；web 测试若本机无浏览器应报 availability 错误而非超时挂死——若挂死，这就是要修的点。

- [ ] **Step 3: 修到全绿**

在文件打开、解析器入口、子进程/浏览器连接处补错误转换；解析器对越界输入返回错误。不改动工具语义，只把 panic/挂起变成结构化错误。

- [ ] **Step 4: 全量测试**

Run: `cargo test -p loong-reverse-engine`
Expected: 全绿。

- [ ] **Step 5: Commit**

```bash
git add crates/reverse/vendor
git commit -m "fix(reverse): structured errors for bad targets and missing browser"
```

### Task 5: `loongagent reverse` 命令

**Files:**
- Create: `crates/cli/src/reverse.rs`
- Modify: `crates/cli/src/main.rs`（`main` 里 `"reverse"` 分支）
- Test: `crates/cli/tests/reverse_cmd.rs`（若 cli 测试惯例是模块内则随文件）

**Interfaces:**
- Consumes: `crates/cli/src/web.rs` 的 `WebSession` 桥模式（ChildTransport + cwd 填充）、`loong_web::{random_token, bind}`、`McpSet`（Task 2）、`Profile::explain_all`。
- Produces: `pub fn run(target: &str, cwd: &Path, port: u16) -> Result<()>`；CLI 用法 `loongagent reverse <target> [--cwd <dir>] [--port <n>]`。`.reverse/` 骨架的创建与清理语义（spec §3）：启动时建目录、写 `.gitignore: *`、写/覆盖 `mcp.toml` 并自 trust；退出（含 SIGINT）删 `mcp.toml` 与 `mcp.trusted`，其余目录内容保留。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn reverse_scaffold_creates_gitignored_dirs_and_manifest() { /* tmp cwd 调 scaffold()，断言目录、.gitignore 内容、mcp.toml 含 name="loong-reverse"、mcp.trusted 存在 */ }

#[test]
fn reverse_cleanup_removes_manifest_but_keeps_study() { /* 造一个 study/00-overview.md，cleanup() 后 manifest 没了、文档还在 */ }

#[test]
fn reverse_rejects_missing_target_before_binding() { /* 不存在的路径 → Err，端口未占用 */ }

#[test]
fn reverse_refuses_name_collision_with_workspace_manifest() { /* .loongagent/mcp.toml 已有 loong-reverse → Err 提示改名 */ }

#[test]
fn reverse_overwrites_stale_manifest_from_killed_run() { /* 预置残留 .reverse/mcp.toml → scaffold 覆盖重写不报错 */ }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p loongagent reverse_`
Expected: FAIL（`reverse` 模块不存在）。

- [ ] **Step 3: 实现**

`reverse.rs` 分三个纯函数加一个编排：`scaffold(cwd) -> Result<ReverseDir>`、`cleanup(cwd)`、`validate_target(target) -> Result<TargetKind>`（`File` | `Url`，文件须存在且是普通文件），`run()` 照 `web.rs` 起服务，Ctrl+C handler 里调 `cleanup`。target 校验失败时在任何 bind/开浏览器之前返回错误。`main.rs` 加 `"reverse"` 分支与 `--cwd`/`--port` 解析（port 默认 0=随机）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p loongagent reverse_`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/cli
git commit -m "feat(cli): loongagent reverse command with .reverse/ scaffold"
```

### Task 6: reverse Web 界面与服务

**Files:**
- Create: `crates/reverse/ui/index.html`
- Create: `crates/reverse/ui/src/lib.rs`（内嵌 html + 服务循环，crate 名 `loong-reverse-ui`）
- Modify: `crates/cli/src/reverse.rs`（改用本 crate 的服务循环）
- Test: `crates/reverse/ui/src/lib.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Consumes: `loong_web::{random_token, bind}`（复用）；Task 5 的 `ReverseDir`。
- Produces: `pub fn serve_reverse(listener: TcpListener, token: &str, reverse_dir: &Path) -> Result<()>`；HTTP 路由：`GET /`（页面）、`GET /ws?token=`（JSON-RPC 桥）、`GET /api/tree`（`.reverse/` 文件树 JSON）、`GET /api/file?path=`（单文件内容）；页面内按钮 `POST /api/phase` body `{"phase":"study"|"build"}` 转发的消息约定见 Step 3。

- [ ] **Step 1: 写失败测试（服务路由）**

在 `lib.rs` 测试中：tmp 目录造 `.reverse/study/00-overview.md`，起服务于随机端口，断言 `GET /` 200 含 `<title`，`GET /api/tree` 的 JSON 含 `study/00-overview.md`，`GET /api/file?path=study/00-overview.md` 返回其内容，`path=../Cargo.toml` 返回 400（路径逃逸拒绝），无 token 的 `GET /ws` 被拒。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p loong-reverse-ui`
Expected: FAIL（crate 不存在）。

- [ ] **Step 3: 实现服务循环与页面**

服务循环 copy `loong_web::serve` 的 accept 循环，加 `/api/tree`、`/api/file`、`/api/phase` 路由（文件读取限制在 `.reverse/` 内，拒绝 `..`）。页面 `index.html` 按 spec §4 布局：顶栏 target 名 + Study/Build 按钮；左侧 `.reverse/` 文件树（2 秒轮询 `/api/tree`）+ markdown 预览；右侧聊天流走 WS JSON-RPC（消息格式照抄 loong-web 的 index.html 里的客户端代码）。Study 按钮：向 WS 发一条 `session.prompt`，文本为 spec §4 的 Study 提示词；Build 按钮同理发 Build 提示词，`00-overview.md` 不存在时前端置灰并提示。markdown 渲染用一百行内的手写转换（标题/代码块/列表即可），不引外部库。

- [ ] **Step 4: 跑测试 + 手工冒烟**

Run: `cargo test -p loong-reverse-ui` → PASS。
再 `cargo run -p loongagent -- reverse /bin/ls --port 0`：浏览器打开、页面三栏可见、`/api/tree` 有内容。截图或 curl 输出留底。

- [ ] **Step 5: Commit**

```bash
git add crates/reverse/ui crates/cli/src/reverse.rs
git commit -m "feat(reverse): web UI with file tree, preview and Study/Build phases"
```

### Task 7: 端到端验收 + 文档收尾

**Files:**
- Modify: `docs/design/security-trust.md`（程序自写 manifest 的 trust 例外一节）、`docs/design/mcp-client.md`（第二 manifest 来源段落）、`docs/design/surfaces.md` 或 `docs/README.md` 索引（reverse 表面一行）
- Test: `crates/cli/tests/reverse_e2e.rs`

**Interfaces:**
- Consumes: 前六个任务的全部产物。
- Produces: spec「验收方式」1-7 全部过。

- [ ] **Step 1: 写端到端测试**

`reverse_e2e.rs`（`#[ignore]` 需要模型的部分不 ignore 的部分必须自动化）：tmp 目录跑 `reverse /bin/ls --port <固定空闲端口>`，用 WS 客户端（测试里手写最小 tungstenite client）发 Study 提示，轮询直到 `.reverse/study/00-overview.md` 出现或 120s 超时；断言 `git status --porcelain` 在该目录为空（`.reverse/` 被 ignore）；kill 进程后断言 `.reverse/mcp.toml` 已删除。若依赖真实模型而不可行，则把「模型驱动」部分降级为手工验收清单写在测试注释里，自动化部分只覆盖 scaffold/serve/cleanup。

- [ ] **Step 2: 跑端到端**

Run: `cargo test -p loongagent --test reverse_e2e`
Expected: PASS（或按注释完成手工项）。

- [ ] **Step 3: 按 spec 验收清单逐项核对**

spec「验收方式」1-7 逐条执行并记录输出；特别做第 6 条（项目自带 `.loongagent/mcp.toml` 已 trust 时 reverse 进出的场景）。

- [ ] **Step 4: 文档与门禁**

补 `security-trust.md`/`mcp-client.md`/索引三处文档；`docs/design/reverse.md` 头部 `Code:` 字段更新为实际文件清单。

Run: `./scripts/check.sh`
Expected: 全绿。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs(reverse): trust exception, second manifest source, e2e acceptance"
```

---

## Self-Review 记录

- **Spec 覆盖**：spec 的六个组成部分（copy 裁剪→T1，mcp 二进制→T3，`.reverse/` 目录与 trust/隔离→T2+T5，Web 界面→T6，会话复用→T5/T6 复用现有机制无需任务，约束与验收→T7）均有归属。Review Focus 六条分别钉进 T2/T4/T5。
- **类型一致性**：`serve_stdio(engine, opts)`、`McpSet::load`、`random_token/bind`、`WebSession`、`Registry::with_all_tools` 均与已核实的源码签名一致。
- **比例**：计划约 200 行对 spec 约 180 行，步骤里无函数体转写。
