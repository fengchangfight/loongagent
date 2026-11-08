Up: docs/README.md
Authority: ADR-0009（one brain, many faces）；本文档拥有 reverse 功能的设计
Code: crates/reverse/（新增）, crates/cli/src/reverse.rs（新增）, crates/cli/src/main.rs
Verify: ./scripts/check.sh

# loongagent reverse：逆向工程模式（最小版）

## 意图

`loongagent reverse <target>` 启动一个本地 Web 界面，让用户对**本地二进制文件**、**网站 URL** 或 **APK 安装包**做逆向工程：先 Study（agent 用逆向工具检查目标，把学习结果写成 markdown 文档），再 Build（用户手动触发，agent 基于 study 文档重建一个自己的版本），全程是普通聊天交互，工具一直可用。

思想借鉴 craft-anything（/home/fengchang/test/craft-anything），但形态大幅简化：coding agent 就是 loongagent 自己，模型用本地配置，不做 study→build→check→ready 的自动 loop，没有 check/ready 阶段。

## 目标 / 非目标 / 反目标

**目标**
- `loongagent reverse <target>` 一条命令起服务、开浏览器、给 URL。
- Study：agent 用逆向工具检查 target，产出 markdown 文档到 `.reverse/study/`。
- Build：用户在网页上点按钮才开始，产出代码到 `.reverse/build/`，之后可多轮聊天迭代。
- 逆向工具集（约 50 个核心工具）从 craft-anything **copy 源码**进本仓库，以 MCP server 形式挂给 session。
- APK 静态分析：解包、组件清单、资源、类/方法检索、smali 字节码查看。

**非目标（本版不做）**
- Check / Ready 阶段（页面不画）。
- iOS 应用、游戏 target。
- APK 的动态分析与外部工具链：JEB 桥、apktool、adb 连真机/模拟器（`android_jeb_*`、`engines_apktool_check`、`adb` toolset 不 copy）。
- Clean room / Deep mode 区分、Ghidra 等外部引擎桥（`crates/engines` 不 copy）。
- 多项目并发、用户配置面板。

**反目标**
- 引入对 craft-anything 的 git/cargo 依赖（用户明确拒绝；工具代码 copy 进仓库）。
- 把 build 做成自动后台流程（必须用户点击触发）。
- `.reverse/` 进 git 跟踪。

## 总体结构

```
loongagent reverse <target>
        │
        ▼
crates/cli/src/reverse.rs
        │  1. 建 .reverse/ 目录骨架（含 .gitignore: *）
        │  2. 写 .reverse/mcp.toml（指向 loong-reverse-mcp 的 stdio 命令）
        │     并调用 trust 逻辑直接信任（这是程序自己写的，不是 repo 内容）
        │  3. 起 HTTP+WebSocket 服务（复用 loong-web 的桥），bind 127.0.0.1 随机端口
        │  4. 打印 URL，调默认浏览器打开
        ▼
浏览器页面（crates/reverse/ui/index.html，单文件无构建步骤）
        │  WebSocket 携带 token，说 JSON-RPC（同 loong-web 的协议）
        ▼
Session（kernel）── attach_mcp 挂上 loong-reverse-mcp
        │                （stdio MCP server，暴露 copy 进来的逆向工具）
        ▼
.reverse/study/*.md   .reverse/build/*   （agent 用写文件工具产出）
```

## 组成部分

### 1. 工具集：核心集 copy（约 40 个，不是 320 个）

用户明确的设计判断：传统 agent 工具是 read_file/write_file/search/execute 这类通用件，而 craft-anything 的价值在于把**逆向工程师的专业操作**抽象成工具——找函数、分析函数、反编译、找调用者/被调用者、追踪参数与数据流、记录假设、验证假设。所以只 copy 承载这些操作的核心工具集，GUI 导航件（cursor/bookmark/comment）、外部工具桥（rizin/cutter/frida/adb）、素材生成、项目管理和 check 阶段的比对工具一律不要。

**保留清单（按 toolset）：**

- **调查记忆（core，~11 个）**：`core_target_open/close`、`core_targets_list`、`core_target_triage`、`core_hypothesis_record/update`、`core_finding_record/edit`、`core_unknown_add/verify`、`core_brief_write`、`core_handoff_write`。这是"记录假设、验证假设"的载体，逆向方法论的骨架。
- **二进制静态分析（native，~16 个）**：`native_functions_list/search`、`native_function_show/disassemble/pseudocode`、`native_function_callers_list/callees_list`、`native_call_graph_show`、`native_call_path_find`、`native_xrefs_list`、`native_strings_browse/find`、`native_sections_list`、`native_bytes_show/find`、`native_value_flow_show`、`native_function_find`。
- **网站检查（browser/js，~11 个）**：`web_site_open/capture/close`、`web_pages_list`、`web_page_show`、`web_page_js_analyze`、`web_page_screenshot_take`、`web_modules_trace`、`web_scripts_save`、`js_source_locate`、`js_sources_restore`。
- **APK 静态分析（android，~8 个）**：`android_package_show`（包信息 + 权限 + 签名）、`android_components_list`（activity/service/receiver/provider）、`android_classes_find`、`android_class_show`、`android_method_show`（smali 字节码）、`android_class_users_find`（谁引用了这个类）、`android_resources_read`（arsc/axml 资源）、`android_flutter_identify`（识别 Flutter 构建及其 Dart 快照提示）。底层解析靠 `formats` crate 里已 copy 的 `dex.rs`/`axml.rs`/`arsc.rs`/`apk_sign.rs`（共约 2500 行纯解析代码，无外部依赖）。
- **辅助（~3 个）**：`core_archive_show/extract`、`core_report_write`。

**明确删除**：cursor/bookmark/comment/label/name_set 系列；`native_cutter_*`、`native_rizin_*`、`native_objdump_run`、`native_source_folder_write`；apple/java/managed/runtime/frida/adb/database/setup/projects/build/application toolset 全部（android 只保留上面列出的 8 个静态分析工具）；`core_person_ask`、`core_action_approve`（loongagent 自己的聊天和权限机制替代）；`assets_*`；`core_investigation_show` 等 GUI 汇总视图（调查状态由 study 文档承担）。

**crate copy 范围相应缩小**到 `crates/reverse/vendor/`：

| 原 crate | copy 后 | 裁剪 |
|---|---|---|
| `crates/evidence` | `loong-reverse-evidence` | 原样 |
| `crates/platform` | `loong-reverse-platform` | 删掉 PTY/进程注入等 runtime 工具才用的部分 |
| `crates/formats` | `loong-reverse-formats` | 原样（纯解析器，留着兜底） |
| `crates/disasm` | `loong-reverse-disasm` | 原样 |
| `crates/js` | `loong-reverse-js` | 原样 |
| `crates/analysis` | `loong-reverse-analysis` | 删掉比对/trace 中只服务 check 阶段的部分 |
| `crates/engine` | `loong-reverse-engine` | 注册表和 ToolSpec 契约原样；只注册上面保留的工具；删 inbox/questions/feed/wasm_plugins/session 项目格式 |
| `crates/mcp` | `loong-reverse-mcp`（库） | stdio server 原样，prompts 按保留工具重写 |

**网站工具的浏览器依赖**：`web_*` 工具需要一个本机浏览器（CDP 连接）。craft-anything 的处理是连接用户已开的浏览器或自己起一个 headless 实例；copy 时保留其原有探测逻辑，不可用时工具返回明确的 availability 错误，不影响二进制分析路径。

**APK 工具的边界**：`android_*` 静态工具是纯 Rust 解析（zip + dex + axml + arsc），无外部依赖；Dalvik 字节码以 smali 文本呈现，不做 Java 级反编译——那需要 JEB/apktool（本版明确不 copy）。Study 提示词要按 target 类型分流：APK 走 `android_package_show` → `android_components_list` → 按需 `android_classes_find`/`android_method_show`/`android_resources_read` 的路径。

### 2. `loong-reverse-mcp` 二进制

新增 bin crate `crates/reverse/mcp-server/`（包名 `loong-reverse-mcp`）：main 函数约 30 行，构造 Engine（target 从 argv 或首条工具调用传入），调 `loong-reverse-mcp` 库的 `serve_stdio`。编译进同一个 `loongagent` 构建流程，安装时作为兄弟二进制放在 loongagent 旁边。

### 3. `.reverse/` 目录

在 `--cwd`（默认当前目录）下创建：

```
.reverse/
  .gitignore        # 内容一行：*
  mcp.toml          # reverse 专用的 MCP manifest（只有 loong-reverse 一个 server）
  mcp.trusted       # 它的 trust 记录（程序自写，见下）
  session/          # 会话日志（沿用 loongagent 现有 session 机制）
  study/            # agent 在 Study 阶段写的 markdown
  build/            # Build 阶段产出的新项目代码
```

`.reverse/.gitignore: *` 保证整个目录不进 git（用户要求）。

**为什么不放 `.loongagent/mcp.toml`**：项目自己的 MCP 配置就在那里（一个 `[[server]]` 表一个 server，按 `name` 区分，多个共存），reverse 追加一个表本不冲突；但 trust 记录是**对整个 `mcp.toml` 字节的 hash**——改一个字，文件里所有 server（包括项目自己已 trust 的）全部失效，要重新 `loongagent mcp trust`。reverse 进进出出改这个文件会反复打翻项目自己的 trust，不可接受。所以 reverse 用 `.reverse/mcp.toml` 这一对独立文件，项目的 manifest 一个字节不碰。

**加载合并**：`McpSet::load` 扩展为读两个来源——`.loongagent/mcp.toml`（照旧，人工 trust gate）和可选的 `.reverse/mcp.toml`（reverse 程序自写自 trust，信任理由见下）。两边 server 名不能撞：reverse 固定用 `loong-reverse`，若项目 manifest 里已有同名 server 则启动报错让用户改名。工具名命名空间规则不变。这个合并逻辑写在 `loong-mcp`，`attach_mcp` 与 `mcp list` 自然看到合并后的集合；`mcp list` 输出里标明每个 server 来自哪个文件。

**trust**：reverse 写完 `.reverse/mcp.toml` 后直接对它的字节调 trust 记录逻辑写 `.reverse/mcp.trusted`。理由：这个 manifest 是 loongagent 自己生成的、指向自己装的二进制，不是 repo 内容，绕过人工 trust 不破坏 `security-trust.md` 的边界（repo 内容的 trust gate 照旧）。实现时在 `docs/design/security-trust.md` 加一节说明，并在 `docs/design/mcp-client.md` 补"第二个 manifest 来源"的段落。

**生命周期与隔离（用户明确要求）**：逆向工具只允许出现在 reverse 会话里，不得影响普通工作的工具选择。靠两条保证：

1. `.reverse/mcp.toml` 只由 `loongagent reverse` 写入；普通 `loongagent` 会话不读 `.reverse/` 这个来源（合并逻辑由 reverse 的启动路径开启，或者：普通会话也读但 reverse 退出时删除文件，文件不在即无 server——实现时二选一，取改动小的，并在 `mcp-client.md` 写明）。
2. `reverse` 退出时（含 Ctrl+C 的信号处理路径）删除 `.reverse/mcp.toml` 与 `.reverse/mcp.trusted`；启动时发现残留（上次异常退出）直接覆盖重写。`.loongagent/` 下项目自己的任何文件全程不被触碰。。

**mcp.toml 与 trust**：`reverse` 命令写完 manifest 后直接调用 `loong_mcp` 的 trust 记录逻辑写 `mcp.trusted`。理由：这个 manifest 是 loongagent 自己生成的、指向自己装的二进制，不是 clone 下来的 repo 内容，绕过人工 trust 不破坏 `security-trust.md` 的边界（repo 内容的 trust gate 照旧）。这一点在实现时要在 `docs/design/security-trust.md` 加一节说明。

### 4. Web 界面

`crates/reverse/ui/index.html`，单文件、无构建步骤、无 CDN（沿用 loong-web 的约束），由 reverse 的 HTTP 服务内嵌提供。布局模仿 craft-anything 桌面版但简化：

```
┌──────────────────────────────────────────────────┐
│ target: /path/to/foo.elf        [Study] [Build]  │  ← 顶栏：阶段按钮
├────────────────────┬─────────────────────────────┤
│ .reverse/          │                             │
│  study/            │        聊天区                │
│   00-overview.md   │   （流式输出，工具调用可见）  │
│   01-arch…md  ◄点击│                             │
│  build/            │                             │
│   src/main.rs      │                             │
├────────────────────┴─────────────────────────────┤
│ 文件预览（点左侧文件名在此显示）    │ [输入框] [发送] │
└──────────────────────────────────────────────────┘
```

- 左上是 `.reverse/` 的文件树，点击文件在左下预览（markdown 渲染）。
- 右侧是聊天流，和 loong-web 一样走 WebSocket JSON-RPC，流式显示。
- **Study 按钮**：向 session 注入一条固定提示词（见下），开始 study。可重复点（继续/补充 study）。
- **Build 按钮**：不自动触发（用户要求）；点击后注入 build 提示词。Study 还没产出 overview 时按钮置灰并提示。
- 文件树通过 HTTP 接口轮询（每 2 秒），不走 WebSocket。

**Study 提示词**（注入给用户不可见的 system 追加 + 可见的用户消息）要点：
1. 用 `core_target_open` 打开 target；用 `core_target_triage` 判断类型，再按类型分流：二进制走 native 工具集（formats/disasm）；网站走 web 工具集；APK 走 `android_package_show` → `android_components_list` → 按需 `android_classes_find`/`android_method_show`/`android_resources_read`（APK 的字节码是 smali，没有 Java 级反编译，不要假装能给出 Java 源码）。
2. 按需 `enable_toolset` 开 native/web/android 工具集深入。
3. 边查边把结果写进 `.reverse/study/`：`00-overview.md`（是什么、怎么运行、技术栈）、`01-architecture.md`（结构拆解）、`02-findings.md`（关键发现，引用 evidence id）。用普通写文件工具写。
4. 完成后在聊天里给一段不超过 10 行的中文总结。

**Build 提示词**要点：读 `.reverse/study/` 全部文档，在 `.reverse/build/` 里重建一个功能等价的项目（技术栈让用户在点击时附言指定，未指定则由 agent 选合理默认），之后进入普通聊天迭代。

### 5. 会话与模型的复用

reverse 的 session 就是普通 loongagent session：模型用 `~/.loongagent/` 里已配置的，记忆、权限、sandbox 全部照旧。区别只在三件事：工作目录下挂了 reverse 的 MCP server、页面布局不同、注入了 reverse 的系统提示。HTTP/WS 服务复用 `loong_web::{random_token, bind, serve}`，`crates/cli/src/web.rs` 里的 `WebSession` 桥（ChildTransport + cwd 填充）照抄一份改成 reverse 版。

## 约束

- 遵守本仓库的模块契约：每个新 `.rs` 文件带 `//!` 模块契约（含 `Design: docs/design/reverse.md` 即本文档），`scripts/check-modules.py` / `check-docs.py` 必须过。
- copy 进来的 vendor crate 保留其原有代码风格与 lint 配置，但文件级模块契约仍要补（可用脚本批量加最简契约头）。
- 不引入新的重型依赖：HTTP/WS 复用 loong-web 已在用的（tungstenite 等）。
- bind 127.0.0.1 + 随机端口 + token 握手，安全模型同 ADR-0009。

## 验收方式

1. `cargo build` 后 `loongagent reverse /bin/ls` 能起服务、开浏览器。
2. 点 Study，聊天区流式输出，`.reverse/study/00-overview.md` 等文件出现并可在左侧预览。
3. 点 Build，`.reverse/build/` 出现新项目骨架；之后发消息能多轮迭代。
4. `loongagent reverse https://example.com` 同样走通（用网站检查工具）。
5. `loongagent reverse app.apk` 走通：`android_package_show` 能给出包名/权限/组件清单，study 文档按 APK 路径产出。
6. `git status` 不显示 `.reverse/` 内容。
7. 项目已有自己的 `.loongagent/mcp.toml` 且已 trust 时：reverse 会话里两边的工具同时可用；reverse 退出后项目 manifest 字节未变、trust 仍有效，普通会话里没有逆向工具。
8. `./scripts/check.sh` 全绿。

## 留给实现者的决定

- 具体 copy 哪些工具文件、删哪些：以"二进制 + 网站 + APK 三类 target 的 core/native/web/js/android 工具集"为准，实现时按 `enable_toolset` 列表清点；`engine` 对 `craft-anything-engines` 的依赖整层剥掉。
- 文件树轮询接口的具体形状（一个 `GET /api/tree` 返回 JSON 即可）。注意 `loong_web::serve` 目前只服务内嵌的 `INDEX` 和 WebSocket，reverse 版需要在自己的服务循环里加 `/api/tree`、`/api/file` 两个路由，可以 copy `serve` 改，不动 `loong-web` 本身。
- 普通会话是否也读 `.reverse/mcp.toml` 这个来源：二选一（reverse 启动路径才开启合并，或普通会话也读但靠退出删除保证为空），取对 `loong-mcp` 改动小的方案，写进 `mcp-client.md`。
- vendor crate 的契约头批量补写方式。
- Study/Build 提示词的措辞（按上面要点写，实现时可以调）。
