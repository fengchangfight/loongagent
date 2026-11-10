# loongagent reverse：逆向工程模式（最小版）

- **Up:** [design/README.md](README.md) · [../04-system-overview.md](../04-system-overview.md)
- **Authority:** [ADR-0009](../decisions/0009-surfaces.md)
- **Code:** `crates/reverse/vendor/`（随源码 vendor 的逆向 crate）, `crates/reverse/vendor/loong-reverse-mcp/src/bin/loongagent-reverse.rs`（MCP server 入口）, `crates/reverse/ui/`（门户里的 study 表面）, `crates/cli/src/reverse.rs`, `scripts/check-reverse.sh`
- **Verify:** `./scripts/check.sh`（含 `scripts/check-reverse.sh`）

## 意图

`loongagent reverse <target>` 启动一个本地 Web 界面，让用户对**本地二进制文件**、**网站 URL** 或 **APK 安装包**做逆向工程：先 Study（agent 用逆向工具检查目标，把学习结果写成 markdown 文档），再 Build（用户手动触发，agent 基于 study 文档重建一个自己的版本），全程是普通聊天交互，工具一直可用。

思想借鉴 craft-anything（/home/fengchang/test/craft-anything），但形态大幅简化：coding agent 就是 loongagent 自己，模型用本地配置，不做 study→build→check→ready 的自动 loop，没有 check/ready 阶段。

## 目标 / 非目标 / 反目标

**目标**
- `loongagent reverse <target>` 一条命令起服务、开浏览器、给 URL。
- Study：agent 用逆向工具检查 target，产出 markdown 文档到这次 run 的 `.reverse/study/<项目>/`。
- Build：用户在网页上点按钮才开始，产出代码到 `.reverse/build/<项目>/`，之后可多轮聊天迭代。
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
        │  1. 建 .reverse/ 骨架（含 .gitignore: *）选定当前项目，再占一个 run 号（run-001、run-002……）
        │     → .reverse/study/<项目>/、.reverse/build/<项目>/、.reverse/runs/<run>/
        │  2. 写 .reverse/runs/<run>/mcp.toml（指向 loong-reverse-mcp 的 stdio 命令）
        │     并调用 trust 逻辑直接信任（这是程序自己写的，不是 repo 内容）
        │  3. 起门户服务（loong_web::serve_with + 本研究表面），bind 127.0.0.1 随机端口
        │  4. 打印 /reverse URL，调默认浏览器打开
        ▼
浏览器页面（门户：/ 是聊天 tab，/reverse 是 study tab；crates/reverse/ui/）
        │  WebSocket 携带 token，说 JSON-RPC（同 loong-web 的协议）
        ▼
Session（kernel）── attach_mcp 挂上 loong-reverse-mcp
        │                （stdio MCP server，暴露 copy 进来的逆向工具）
        ▼
.reverse/study/<项目>/*.md   .reverse/build/<项目>/   （agent 用写文件工具产出）
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

在 `--cwd`（默认当前目录）下创建。顶层分两件事：**项目**给人看，**run** 记进程。

```
.reverse/
  .gitignore         # 内容一行：*
  project            # 当前项目的名字（一行）；下次启动接着用它，没有就取最近改过的项目
  projects/<项目>/    # target / target-kind / phase / session：这套状态跟着项目走
  study/<项目>/      # 这个项目的研究笔记（markdown），以及 evidence/ 里的中间产物
  build/<项目>/      # 这个项目的重构产物
  runs/run-001/
    session          # 写这些笔记的会话 id（~/.loongagent/projects/<project>/<id>.jsonl）
    mcp.toml         # 这次进程专用的 MCP manifest（只有 loong-reverse 一个 server）
    mcp.trusted      # 它的 trust 记录（程序自写，见下）
  .trash/<项目>-<时间戳>/  # 重做研究时挪走的旧笔记
```

`.reverse/.gitignore: *` 保证整个目录不进 git（用户要求）。

**项目名就是输出目录名（用户要求）**：笔记在 `study/<项目>/`，重构产物在 `build/<项目>/`。要研究别的东西就新建项目：页面换到干净的一页，旧项目的笔记原地不动、随时切回。名字允许中文和空格，不许 `/`、`\`、`..` 和开头的点，最长 64 字符；没有项目时自建 `project-001`。

**"这个项目已经研究过了"要有明确答案**：`study/<项目>/` 里除 `evidence/` 外有文件就算研究过。再点「开始研究」
不会静悄悄覆写：页面先问"本项目已经做过研究了，重做会覆盖之前的笔记，继续吗？"，同意后把旧的挪
进 `.trash/`，再写新的。判据是有笔记，不是 `phase`——`phase` 是人肉的「研究完成」闸门（§4）；
只解了包、还没动笔的项目不算研究过。

**删除项目是真删（用户要求）**：`delete:<项目名>` 把 `projects/`、`study/`（连 `evidence/`）、`build/` 一起
`remove_dir_all`，并清掉 `.trash/<项目名>-<时间戳>` 里它以前的旧笔记——留着就不算"彻底删除"（`run` 不碰
`run-001` 的，只有"名字-时间戳"才算）。删的是当前项目时会话交给还剩的第一个，一个不剩就删掉
`.reverse/project`；页面要先打一遍项目名才动手（`reverse-ui.md`）。

**run 记的是进程**：`run-001`、`run-002`……由目录名原子占位（`create_dir` 成功即拿到号），两个进程同时起不会撞号。`session` 把 run 号与会话 id 对上：会话日志仍在 `~/.loongagent/projects/<project>/<id>.jsonl`（沿用普通机制，不在 `.reverse/` 另存），想知道笔记是哪次对话写的读 `runs/<run>/session`；想知道这个项目**现在**的会话是哪个，读 `projects/<项目>/session`（§5）。manifest 那一对也留在 run 里，理由见下。

**同一项目并行跑两次会互相覆盖**：两个进程写同一份 `study/<项目>/`，这是"项目名就是输出目录名"的代价，本版接受：要并行就起两个项目。**笔记目录里只有笔记**——`runs/<run>/` 那种台账、解包出来的文件和抓取结果都不算笔记（后两者只进 `study/<项目>/evidence/`）；页面列文件时照实列 `evidence/`，但 `phase`、`session` 这类台账不会混进研究结果。

**为什么不放 `.loongagent/mcp.toml`**：项目自己的 MCP 配置就在那里（一个 `[[server]]` 表一个 server，按 `name` 区分，多个共存），reverse 追加一个表本不冲突；但 trust 记录是**对整个 `mcp.toml` 字节的 hash**——改一个字，文件里所有 server（包括项目自己已 trust 的）全部失效，要重新 `loongagent mcp trust`。reverse 进进出出改这个文件会反复打翻项目自己的 trust，不可接受。所以 reverse 用自己 run 目录里那一对独立文件，项目的 manifest 一个字节不碰。

**加载合并**：`McpSet::load` 读两个来源——`.loongagent/mcp.toml`（照旧，人工 trust gate）和 reverse 那一份（路径由 `LOONGAGENT_REVERSE_DIR` 指到进程自己的 `runs/<run>/`；没有这个变量的会话就没有第二个来源）。每个 server 带 origin（`workspace` / `reverse`），`mcp list` 逐条标明。两边 server 名不能撞：撞名是响亮的 duplicate 错误，指出撞在哪个文件，不是静默覆盖。trust 判断**按源文件各自独立**：reverse 那份的字节对不上自己的记录时，只有 reverse 的 server 进入 inert，提示信息点名那个 manifest；项目 manifest 的判定纹丝不动。合并逻辑写在 `loong-mcp`，`attach_mcp` 与 `mcp list` 自然看到合并后的集合。

**按 run 分开：为什么路径要跟着进程走**。manifest 是按 run 写的，所以「第二个来源」不能是编译期写死的一个路径。`loongagent reverse` 在 `runs/<run>/mcp.toml` 写好 manifest 后，把该目录通过环境变量 `LOONGAGENT_REVERSE_DIR` 交给它自己起的 server 子进程（`crate::web::make_session` 起的那个），`McpSet::load_for_process` 读到它就去读那一份。这样做有两个好处：

1. **并行不打架**：两个 reverse 进程各有一个 manifest，A 退出只删 `runs/A/` 下的对，B 的工具照旧；旧布局下一个进程退出就会把另一个进程正在用的 manifest 删掉。
2. **隔离更硬**：普通 `loongagent` 会话没有这个环境变量，因此**永远**读不到 reverse 的 manifest——即便此刻正有 reverse 会话在同一个项目里跑。旧布局做不到这一点：普通会话与 reverse 共用 `.reverse/mcp.toml` 这个路径，只能靠「reverse 退出时删文件」在时间上错开。

**mcp.toml 与 trust**：`reverse` 写完 `runs/<run>/mcp.toml` 后直接调 `McpSet::trust_reverse_now` 写同一目录的 `mcp.trusted`。理由：这个 manifest 是 loongagent 自己生成、指向自己装的二进制，不是 repo 内容，绕过人工 trust 不破坏 `security-trust.md` 的边界（repo 内容的 trust gate 照旧）。`loongagent mcp trust` 只管项目那一对；run 目录里的那对由 reverse 在写出时自行信任，故该命令不去替它作答。

**生命周期与隔离（用户明确要求）**：逆向工具只许出现在 reverse 会话里。`runs/<run>/mcp.toml` 只由 `loongagent reverse` 写、只由它起的 server 子进程读（`LOONGAGENT_REVERSE_DIR` 只发给那棵进程树）；`reverse` 退出时（含 Ctrl+C 的信号路径）删**自己 run 目录里**的那一对。旧布局的 `.reverse/mcp.toml` 由新版本启动时顺带删除，被 `kill -9` 留下的 run 目录里的那对没人读，也不影响任何人。

**"要不要跑"这一问归宿主**：manifest 里给 server 传 `args = ["--allow-run"]`。reverse MCP server 对"跑代码 / 改设备"的工具（`web_site_open` 等，判据是 `Effects::needs_consent()`）默认回 `consent_required`，它自己的问法是往 inbox 里塞一张批准请求（`loong-reverse-mcp/src/lib.rs`，只在 `InboxDir` 在场时才问）；这个会话没有 inbox，问不出来。所以不传这个开关时，页面上点「允许一次」只放行了宿主这一层（reverse 的工具没声明 `action_class`，按 MCP 默认是 `out_of_bounds`，每次调用都问），工具照样回 `consent_required`，模型重试一次又弹一张卡片、又同样失败——网站目标永远打不开。

传了不减少任何一次询问：同意仍然要在页面上给，每次调用都给一次（`security-trust.md` 的阶梯不变）。它只保证"给完就真的会跑"。要减少询问，得在 manifest 里给工具声明 `action_class`（只读的 `investigate`，写 `.reverse/` 的 `write_workspace`），本版不做：那份清单会随 vendor 工具集变化，写死在生成器里迟早说谎。

### 4. Web 界面：portal 里的一个 tab

**门户统一（用户明确的决定）**：`loongagent reverse` 不再自带一套服务，而是**复用 `loongagent web` 的同一个门户**——一个监听、一个 token、一张页面。reverse 只是往门户里挂一个 tab：

```
http://127.0.0.1:<port>/#token=<token>       ← Chat tab（loong-web 原有页面）
http://127.0.0.1:<port>/reverse#token=<token> ← Reverse study tab
http://127.0.0.1:<port>/ws?token=<token>     ← 一个 WebSocket，两个 tab 共用
```

页面上的 tab 切换就是换路径：Chat tab 指向 `/`，Study tab 指向 `/reverse`，token 跟着 URL fragment 一起带过去。以后再有新表面（比如别的模式），照这个形状再加一个 tab 与一个前缀，不新增监听端口。

实现上 `loong-web` 有一个很小的扩展点：

```rust
pub trait Router: Send + Sync {
    fn prefix(&self) -> &str;                               // 例："/reverse"
    fn answer(&self, head: &str, stream: &mut TcpStream) -> Result<(), String>;
}
pub fn serve_with(session, listener, token, router: Option<Arc<dyn Router>>);
```

`serve_with` 先看请求路径：命中某个 `Router` 的前缀就交给它，`/ws` 走门户的 WebSocket（token 校验不变），其余回门户自己的聊天页。`serve()` 就是 `serve_with(..., None)`，`loongagent web` 的行为一个字节不变。这正是"一个门户、多个表面"：门户管监听、token、聊天；表面管自己那块 URL。

**页面文件**：`crates/reverse/ui/`（包 `loong-reverse-ui`），两个文件——`lib.rs`（路由 + 文件树 JSON + 文件弹窗 + `StudySurface: Router`）、`page.html`（单文件、无构建步骤、无 CDN，沿用 loong-web 的约束）。

**页面本身（布局、目标栏的两个标签、研究→重构那条工作流、各路由的报文格式）在 [reverse-ui.md](reverse-ui.md)**；本节只留引擎这一侧的约束，两条是硬的：

- **界面中文，提示词英文**：给人看的字全用中文；注入给模型的提示词仍是英文，模型对英文指令更稳，这层翻译没有收益。
- **重构不自动触发，由用户点「标记研究完成」确认**：不看 `.reverse/study/<项目>/` 里有没有文件。模型可能只写了一半就落盘，自动点亮会让用户在没研究完时就开始重构。

#### 4.1 目标是界面上设的，命令行不带

`loongagent reverse` **不接受位置参数**，打命令就起门户，目标在页面顶栏里选。

理由不是"少打字"，是 `reverse <target>` 这个参数原本就没接线：MCP server 启动时不接收 target（`crates/reverse/mcp-server/src/main.rs` 首行注释："with no pre-opened target: the session's first `core_target_open` chooses it"）。会话里的 target 只由模型调用 `core_target_open` 时决定。所以旧参数校验完路径就丢掉了，模型根本不知道你给了什么。

**两条入口，别混**：目标栏只有「本地目标」和「网站」两个标签，因为第一个工具调用只有两种。

| 标签 | 入口工具 | 说明 |
|---|---|---|
| 本地目标（文件或文件夹） | `core_target_open(绝对路径)` | 打开后是什么由 `core_target_triage` 嗅探内容决定 |
| 网站 | `web_site_open(http(s)://…)` | **不走 `core_target_open`**：`TargetKind` 里没有任何 URL 变体 |

本地目标是 ELF/PE/Mach-O、APK、还是别的压缩包，不归页面判断：`core_target_open` 打开后由 `core_target_triage` 按内容（`sniff`）定类型，并回 `recommended_toolsets`——二进制给 native，APK 给 android。APK 曾经有自己的标签，但那只是提示词里先说一句"走 android"：入口、校验、存储完全一样，而标签本身猜不准没写 `.apk` 后缀的包（`.aab`、`.xapk`、`.dex`、改名的 zip、套在文件夹里的 apk 都只能走本地目标），所以并成一个。

网站走的是另一条链路：`web_site_open` 起一个会话私有的浏览器（干净 profile、无历史无登录）打开页面，之后的 `web_pages_list`/`web_page_show`/`web_modules_trace`/`web_scripts_save` 都在这个页面上工作，`web_site_capture` 把整页截图、渲染后 HTML、设计令牌、断点与布局、素材清单、链接和表单轮廓落到一个文件夹，`web_site_close` 收尾。所以"网站目标"的产物和本地目标不在同一个地方——一个在浏览器里，一个在 `.reverse/` 里——study 笔记必须写清证据来自哪。

网站链路依赖 Chromium（`Requires::Engine("chromium")`）。找不到浏览器时 `web_site_open` 返回明确的可用性错误，不影响本地目标。`locate.rs` 认的不只是 `chromium` 这个名字，`google-chrome` 等真实二进制也算（`loong-reverse-engine/src/locate.rs`）——实测本机只有 `/usr/bin/google-chrome` 也能用。

目标怎么输入、怎么校验、报文长什么样，都在 [reverse-ui.md](reverse-ui.md)：那里是本页面的唯一权威。

#### 4.2 表面数据路由一律要 token

门户的 `/`（聊天页）与 `/reverse`（study 页）本身不含秘密，可以无 token 取；但**表面的数据路由**（`/reverse/tree`、`/reverse/file/…`、`/reverse/ls`、`/reverse/target`、`/reverse/phase`、`/reverse/run`、`/reverse/projects`、`/reverse/study/redo`、`/reverse/interrupt`）必须校验 token，由 `serve_with` 统一把关：命中 surface 前缀且不是页面本身时，先查 `x-loong-token` 头或 `token` 查询参数，不对回 401，再交给 surface。页面从 URL fragment 拿到 token，用它发 fetch。

这条是目录浏览器的前提：能列整个文件系统的接口不能裸奔在 loopback 上。

**Study 提示词**（注入给用户不可见的 system 追加 + 可见的用户消息）要点，措辞用英文：
1. 按目标类型分流，第一步就不同：本地目标（文件或文件夹，含 APK）用 `core_target_open` 打开，再用 `core_target_triage` 判断内容并照它给的建议开工具集；网站用 `web_site_open` 打开网址。**不要**对网址调 `core_target_open`（它只接受绝对路径）。
2. 分流之后：二进制走 native 工具集（formats/disasm）；网站走 web 工具集，用 `web_site_capture` 落证据，`web_site_close` 收尾；APK 走 `android_package_show` → `android_components_list` → 按需 `android_classes_find`/`android_method_show`/`android_resources_read`（APK 的字节码是 smali，没有 Java 级反编译，不要假装能给出 Java 源码）。
3. 按需 `enable_toolset` 开 native/web/android 工具集深入。
4. 边查边把结果写进当前项目的笔记目录 `.reverse/study/<项目>/`（`<项目>` 由页面从 `/reverse/run` 拿到，写进提示词）：`00-overview.md`（是什么、怎么运行、技术栈）、`01-architecture.md`（结构拆解）、`02-findings.md`（关键发现，引用 evidence id）。用普通写文件工具写。网站目标的笔记里要写清证据来自哪个 capture 文件夹。**中间产物一律落在 `study/<项目>/evidence/<名字>/`**——解包出来的文件、保存的脚本、trace、`web_site_capture` 的抓取结果（抓取工具要求父目录已存在，新建项目时就把 `evidence/` 建好了）；`build/<项目>/` 只留给重构，研究阶段不往里写。
5. 完成后在聊天里给一段不超过 10 行的中文总结。**不要**自己宣布"研究完成"——那一步由用户在页面上点，见 reverse-ui.md。

**Build 提示词**要点：读当前项目的 `.reverse/study/<项目>/` 全部文档（笔记指向的原料在 `study/<项目>/evidence/`，按需去看），在 `.reverse/build/<项目>/` 里重建一个功能等价的项目（技术栈让用户在点击时附言指定，未指定则由 agent 选合理默认），之后进入普通聊天迭代。

### 5. 会话与模型的复用

reverse 的 session 就是普通 loongagent session：模型用 `~/.loongagent/` 里已配置的，记忆、权限、sandbox 全部照旧。区别只在四件事：工作目录下挂了 reverse 的 MCP server、多一个 `/reverse` 表面、注入了 reverse 的系统提示、**一个项目默认接着上次那段对话**。

最后一条是为了接着上次聊：会话 id 记在 `projects/<项目>/session`（和 target、phase 放一起），启动时先读它——日志还在就直接接上。所以同一个项目无论隔一天还是一周再打开，历史都在，页面连上就从第一条重放（[reverse-ui.md](reverse-ui.md)、[reverse-conversation.md](reverse-conversation.md)）。这条记录是"下次打开哪个"，不是"只有这一个"：一个项目可以有多份会话，页面把它们列出来，随手切换或新开一个；换过之后这条记录跟着更新。这些会话存在**项目自己的抽屉**里——`~/.loongagent/projects/reverse-<项目>-<哈希>/`——不在工作区那份目录（`~/.loongagent/projects/<工作区>/`）里：同一个工作区可以有好几个逆向项目，而那份目录是 TUI、`loongagent resume`、VS Code 和别的逆向项目共用的，一个用了几个月的工作区里躺着几十段跟这次研究无关的对话。抽屉名由工作区路径 + 项目名算出来，`ls ~/.loongagent/projects` 一眼能看出哪些是逆向项目、各属于哪个项目；服务端子进程拿 `--session-drawer <名字>` 在抽屉里建、开、列，所以页面上的列表天生只含本项目的会话，不需要过滤。代价是 TUI 的 `resume` 也列不到这些会话——它们属于这个研究，续在页面上。改版之前已经绑过的会话，第一次启动会被搬进抽屉（`sessions` 清单只在这一步读，之后不再写）（[reverse-conversation.md](reverse-conversation.md)）。`--session <id|latest>` 或 `--new` 是命令行上的同一个选择。为此 `reverse` 命令不再用"默认新会话"的解析（`Args::parse_fresh`）——那会在读记录之前就把 id 定死，等于每次启动都丢历史。日志只有一把写锁：正在被另一个进程写的那份打不开，这次启动另开一个会话并在终端说清（记录不动，因为那份对话属于正在写的进程），页面照样能开，不去抢那把锁。`runs/<run>/session` 记着每个 run 用的是哪个会话（sessions-lineage.md §5）。

会话的构造就是 `crates/cli/src/web.rs` 里那一个 `make_session`（`web` 与 `reverse` 共用，不再复制第二份 `WebSession` 桥），HTTP/WS 服务用 `loong_web::{random_token, bind, serve_with}`。

## 约束

- 遵守本仓库的模块契约：每个新 `.rs` 文件带 `//!` 模块契约（含 `Design: docs/design/reverse.md` 即本文档），`scripts/check-modules.py` / `check-docs.py` 必须过。
- copy 进来的 vendor crate 保留其原有代码风格与 lint 配置，但文件级模块契约仍要补（可用脚本批量加最简契约头）。
- 不引入新的重型依赖：HTTP/WS 复用 loong-web 已在用的（tungstenite 等）。
- bind 127.0.0.1 + 随机端口 + token 握手，安全模型同 ADR-0009。

## 验收方式

1. `cargo build` 后 `loongagent reverse`（不带参数）能起服务、开浏览器；目标在页面顶栏选定。
2. 点 Study，聊天区流式输出，`.reverse/study/<项目>/00-overview.md` 等文件出现，点文件名弹窗看。
3. 点 Build，`.reverse/build/<项目>/` 出现新项目骨架；之后发消息能多轮迭代。
4. 目标栏两个标签都能用：本地目标要求绝对路径（可浏览目录，文件或文件夹都行），网站要求 `http(s)://`；选定后写进 `.reverse/projects/<项目>/target` 与 `target-kind`，重开页面时标签和值都回填。
5. 目标是 APK（在「本地目标」里选中）时走通：`core_target_triage` 给 android 工具集，`android_package_show` 能给出包名/权限/组件清单，study 文档按 APK 路径产出。
6. `git status` 不显示 `.reverse/` 内容。
7. 同一个项目里已有另一个 loongagent 进程（TUI 或另一个 reverse）在跑时，`loongagent reverse` 仍能起、能聊、能写笔记；第二次 `loongagent reverse` 拿到的是 `run-002`，两次的笔记互不覆盖，先退出的那个不会删掉后退出那个的 manifest。
7. 项目已有自己的 `.loongagent/mcp.toml` 且已 trust 时：reverse 会话里两边的工具同时可用；reverse 退出后项目 manifest 字节未变、trust 仍有效，普通会话里没有逆向工具。
8. `http://<addr>/#token=…`（Chat）与 `http://<addr>/reverse#token=…`（Reverse study）在同一个端口上都能打开，两个 tab 用同一个 WebSocket；刷新或重启后再打开 `/reverse`，对话仍是这一串，从第一条重放（§5、reverse-conversation.md）。
9. `./scripts/check.sh` 全绿。

## 留给实现者的决定

- 具体 copy 哪些工具文件、删哪些：以"二进制 + 网站 + APK 三类 target 的 core/native/web/js/android 工具集"为准，实现时按 `enable_toolset` 列表清点；`engine` 对 `craft-anything-engines` 的依赖整层剥掉。
- 表面路由的形状：已定为 `loong_web::Router` + `serve_with`（见 §4），`GET /reverse/tree` 返回 JSON、`GET /reverse/file/<path>` 返回字节、`GET /reverse/ls` 列目录、`/reverse/target` 读写目标、`/reverse/projects` 列/切/建项目、`/reverse/study/redo` 把旧笔记挪进 `.trash/`。以后新表面照此加一个前缀，不复制服务循环。
- URL 目标：需要"下载/录屏进本地再 `core_target_open`"的一条链路，本版不做（见 §4.1）。
- 普通会话是否也读 `.reverse/mcp.toml` 这个来源：二选一（reverse 启动路径才开启合并，或普通会话也读但靠退出删除保证为空），取对 `loong-mcp` 改动小的方案，写进 `mcp-client.md`。
- vendor crate 的契约头批量补写方式。
- Study/Build 提示词的措辞（按上面要点写，实现时可以调）。
