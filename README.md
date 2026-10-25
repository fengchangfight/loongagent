# loongagent

- **Authority:** [docs/README.md](docs/README.md)
- **Code:** `crates/`
- **Verify:** `./scripts/check.sh`

一个装在你自己机器上的编码 agent。单个二进制，**会话原文（JSONL 日志）就是唯一真相**，
每一次权限决定都是一条可审计的事件，模型、记忆、会话全部留在本机 —— 不依赖任何服务器也能用。

- **本地版**：`loongagent tui --local`，模型可以是本机 ollama，也可以是你自己的 API key；
  配置、密钥、会话都在 `~/.loongagent/` 下，别人碰不到。
- **控制面**：企业侧用**签名 pack** 下发 profile：包由 Ed25519 签发、本地按固定公钥验签，
  验不过就用不了，模型/端点/密钥不进用户配置。**但本机的 org 层文件
  `/etc/loongagent/profile.toml` 不签名**，它只是管理员拥有的文件，不能叫防篡改。
  两条路共用同一个内核，本地版不需要它。

![loongagent TUI](screenshottui.png)

*状态栏 `[1] deepseek-flash [2] qwen3.7-flash [3] k3 [4] qwen2.5:7b-instruct`：
云端模型和本机 ollama 模型并排，`alt+1..4` 或 `/1../4` 一键切。下面每一行工具调用都带着
`policy: allow by builtin:write_workspace`，顶部的 `[sandbox] filesystem boundary enforced by landlock`
说明进程边界是真在生效，不是声明。*

---

## 三种能力，先看一句话

- **R 模式**（record，默认回滚路径）：不调用任何模型，检索层 + 确定性 digest 全部工作。
- **S 模式**（semantic local，可选装）：在 R 之上用小模型补 L2/L3/L4，`recall` 能回到原文。
- **控制面**（企业下发）：签名 pack + 固定公钥，模型 id、端点、token 不进用户配置。
  细节见 [docs/context/README.md](docs/context/README.md) 与
  [docs/runbooks/control-plane.md](docs/runbooks/control-plane.md)。

三层是同一套存储、同一套引用语义、同一个 API，只改变**谁生成理解层**；模型不可达时按段
降级回 digest，不是整段失效。

---

## 快速开始

只要 Rust stable 和一个 C 编译器（`bundled` 的 SQLite 与 `ring` 需要编译 C）：

```bash
cargo build --release --locked -p loongagent     # -> target/release/loongagent
cargo install --path crates/cli --locked         # -> ~/.cargo/bin/loongagent
export PATH="$HOME/.cargo/bin:$PATH"
```

Windows 用 `powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1`，前置条件、
两种工具链的差别和**目前还没有的 OS 沙箱边界**见
[docs/runbooks/windows-build.md](docs/runbooks/windows-build.md)。

### 跑起来

```bash
loongagent tui --local                 # 在当前目录开一个本地会话
loongagent tui --local --cwd ~/proj    # 指定工程目录
loongagent tui --local --new           # 另起一个新会话（默认续最近一个）
loongagent resume --local              # 重新打开最近一个
loongagent sessions --all              # 列出所有工程下的会话
```

第一次跑且没有任何模型时，会**先弹配置向导**再进会话。不想每次都打 `--local`，就在
`~/.loongagent/local.toml` 里写 `mode = "local"`（用 `loongagent local edit` 改）；这一行
只能写在本地层里，受管的 `profile.toml` 写它会被拒绝，否则一个仓库配置就能把整台机器从公司
策略里"切出去"。

其他界面（同一个内核、同一份会话）：`loongagent web --local`（浏览器，只监听 loopback）、
`loongagent repl --local`（行模式，不需要 TTY）、`loongagent run --local "…"`（无界面跑一轮）、
`loongagent sessions` / `resume [ID]` / `fork [ID]` / `delete ID`。

---

## 配置模型

```bash
loongagent local add              # 列出服务商（本机 ollama 排第一），粘 key，现场探测
loongagent local add ollama       # 直接指定某一个服务商
loongagent local                 # 看配了什么、在状态栏里是几号槽位
loongagent local edit             # 手动改 local.toml（文件注释就是文档）
loongagent local ping             # 每个已配置模型真打一次
loongagent config show            # 每个设置最终生效的值，以及是哪一层给的
```

向导会**现场探测**：能用 `/models` 列模型的就列出来给你选，key 不对会直接告诉你，并且会指出
"你可能拿的是隔壁那个产品的 key"（Kimi Code 编程套餐 / Moonshot 开放平台、Zhipu / Z.ai 这类
同名不同站的坑）。服务商目录见 [DEVELOPMENT.md](DEVELOPMENT.md)；密钥按约定名（如
`LOONG_DEEPSEEK_KEY`，也认 `DEEPSEEK_API_KEY`）从 `~/.loongagent/env` 或环境变量读取，
**永远不写进配置文件**。

`local.toml` 的 `[[models]]` **顺序 = 能力从低到高 = 状态栏 1、2、3…**，第一个是启动默认：

```toml
mode = "local"

[[models]]
name = "qwen2.5:7b-instruct"
provider = "ollama/qwen2.5:7b-instruct"   # 槽位 1：本机 ollama

[[models]]
name = "deepseek-flash"
provider = "deepseek/deepseek-flash"      # 槽位 2：便宜云

[router]
default = "qwen2.5:7b-instruct"
```

`name` 只是状态栏显示名，真正决定打到哪里的是 `provider`；需要自建端点时才写 `base_url` /
`kind`（见 [DEVELOPMENT.md](DEVELOPMENT.md)）。

### 本机 ollama 与分层记忆

摘要模型只认 **ollama 原生接口**（不是 OpenAI 兼容 shim）——兼容层会忽略 `think:false`，
把预算全烧在推理轨迹上。所以只有 `ollama/` 一种写法：

```bash
ollama pull qwen2.5:7b-instruct
loongagent local semantic on ollama/qwen2.5:7b-instruct
loongagent local semantic            # 开关、模型、base_url、模型是不是真的在
loongagent local semantic on --record  # 只要确定性派生（等于显式打开 R 模式）
loongagent local semantic off
```

生效范围是 `--local`。**模型拉不下来 = 退回 R 模式**，不是坏掉：那一段回退成确定性 digest，
并在 F2 面板和 `loongagent context status` 里显示为 degraded。摘要只能补 `rejected` / `why` /
`open_questions` / `conclusion`，每条摘要必须带引用的 `seq` 区间，`loongagent context verify`
可以校验；计算字段和引用不可被模型改写。细节、默认模型与被测数据见
[docs/context/README.md](docs/context/README.md) 和 [docs/context/rollout.md](docs/context/rollout.md)。

---

## 快捷键与常用命令

**切模型**（状态栏里的模型就是 `[[models]]` 的顺序）：

| 键 | 作用 |
|---|---|
| `alt+1` … `alt+9` | 直接跳到第 N 个槽位（最常用） |
| `/1` … `/9` | 同上；给会吞掉 Alt 的终端（tmux、部分 Windows 终端）兜底 |
| `ctrl+n` / `ctrl+p` | 下一个 / 上一个模型，循环 |
| `alt+r` 或 `/r` | 用**当前选中的模型**重发上一条 prompt |

**会话内**：

| 命令 / 键 | 作用 |
|---|---|
| `/help` | 这个会话能做什么（skills、命令、MCP prompt 由服务端列） |
| `/expand` | 展开最后一条工具输出 |
| `F2` | 上下文面板：下一次请求实际会带什么，以及分层记忆的状态 |
| `/new` `/clear` `/reset` | 新开一个会话（旧日志保留） |
| `/copy` | 复制整个会话（OSC 52 + 同时落一个文件路径） |
| `/yolo` | 关掉审批提示（沙箱不受影响，照旧生效） |
| `/logo` `/mouse` | 开关 logo / 把鼠标还给终端 |
| `/exit` `/quit` `/q`，`ctrl-d` | 退出 |
| `ctrl-c` | **有任务在跑 = 中断这一轮**（会话不结束）；空闲 = 退出 |

`/name args` 形式（不是内置命令的）会当作 **skill / 命令**发给服务端，来自
`.loongagent/skills/`、`.loongagent/commands/`、profile 的 `skill_dirs`、已安装插件和
MCP prompt。界面细节（折叠、配色、mark）由 [docs/design/surfaces.md](docs/design/surfaces.md) §3 拥有。

---

## 文件都在哪

| 路径 | 内容 |
|---|---|
| `~/.loongagent/local.toml` | **本地模式**的唯一配置（含 `[context_engine]`/`[summarizer]`）；`$LOONGAGENT_HOME` 可整体挪走 |
| `~/.loongagent/env` | 密钥（Unix 上 `0600`），或直接用环境变量 |
| `~/.loongagent/projects/<工程>/<会话 id>.jsonl` | 会话原文：一行一个事件，**这是真相** |
| `~/.loongagent/plugins/<名字>/` | 用户级插件：一份 checkout + `.loongagent-plugin.toml` |
| `<工程>/.loongagent/` | 工程层：`profile.toml`（只能收紧权限）、`hooks.toml`、`mcp.toml`、`skills/`、`commands/`、`plugins/` |
| `<会话>.sqlite` / `<会话>.context.db` | 事件索引 / 分层记忆，都是可丢弃、可从日志重建的派生物 |
| Windows 上 | `~` 是 `%USERPROFILE%`；机器级配置在 `%PROGRAMDATA%\loongagent\` |

```bash
loongagent sandbox self-test    # 让受约束的子进程去写工作区外，必须被拒绝才算数
loongagent context status       # 分层记忆：模式、摘要模型、L1–L4 计数、worker 是否 degraded
cat <session>.jsonl             # 全部真相
```

---

## 插件：一键装别人的 skills

```bash
loongagent plugin add superpowers      # 内置目录里的名字，不用记 URL
loongagent plugin list                 # 装了什么、来自哪个 commit、占多少 context
loongagent plugin update superpowers   # 拉最新 commit
loongagent plugin remove superpowers   # 卸载
```

也支持 `owner/repo`、完整 git URL、本地目录和 `@分支或tag`；默认装到用户级，`--project`
装到本工程。**只装 skills**：插件里的 hooks、subagents、MCP servers 一律不动——它们各自的
开关是"每个仓库显式 trust"，装内容不该变成绕过它的路。插件可以声明 `bootstrap`，这些 skill
的正文会进每次会话的 system prompt。规则见
[ADR-0018](docs/decisions/0018-plugin-install-and-bootstrap.md) 与
[docs/design/extension-model.md](docs/design/extension-model.md)。

---

## 控制面

设计见 [ADR-0012](docs/decisions/0012-control-plane.md) 和
[ADR-0016](docs/decisions/0016-pack-distribution.md)：控制面签发 profile pack，本地固定公钥、
验签后才用；包里有占位符（`api_key = "env:…"`）而**永远没有钥匙**。本地版不需要它。

```bash
loongagent setup --url https://control.your-firm.internal --public-key <hex>
loongagent profiles          # 有哪些 profile
loongagent use default       # 选一个
```

**还没做完**：本机 org 层文件 `/etc/loongagent/profile.toml` 没有签名（防篡改的是控制面下发、
验签通过后才生效的签名 pack）；policy / context 载荷还没套同一套签名信封；Windows 上的控制面
布局。落地步骤见 [docs/runbooks/control-plane.md](docs/runbooks/control-plane.md)。

---

## 文档

全部设计文档都在 **[docs/README.md](docs/README.md)** 按主题列好了。工具链、crate 地图和
日常命令见 [DEVELOPMENT.md](DEVELOPMENT.md)。

---

## 状态，说实话

**能用**：内核循环、分层配置、会话与 JSONL 日志（含派生 SQLite 索引）、上下文账本、压缩、
权限 + Landlock 沙箱、hooks（shell/HTTP/MCP 三种）、MCP（工具与 prompt，stdio）、
skills/commands、**插件**、六个内置工具（`read_file` / `write_file` / `edit` / `glob` /
`grep` / `shell`，记忆打开时再加一个 `recall`）、四个界面（tui / web / repl / run）、流式回答、
可选的**分层记忆**（R 模式 + S 模式、后台增量、`recall`/`expand`、F2 可见性）和**控制面**
（签名 pack 由固定公钥验签后才进 control 层（高于 org），`loongagent-control` 二进制与 runbook 都在；身份/SSO、
审计、密钥轮换还没做完）。Linux 与 **Windows** 都能构建运行，命令面一致。
测试 737 个通过、11 个忽略。
<!-- test-counts: 737 passed, 11 ignored -->

**没做完，也不假装**：subagents、插件的索引/搜索与逐插件开关（现在用 `disable_skills`）、
第三方内容的签名与信任分级（[ADR-0018](docs/decisions/0018-plugin-install-and-bootstrap.md)
留给 WASM 那一层）、LSP 代码工具、`grep`/`glob` 的 `.gitignore` 感知、MCP 的 HTTP 传输、
多用户会话、`memory.*` 事件回写日志、`recall` 在真实长会话上的验证，以及**Windows 上的 OS
强制边界**——Landlock 是 Linux 的，那里 `sandbox self-test` 会如实报告自己不可用，边界退回
策略而非内核。

- **成本还没有计价**：`usd_micros` 目前恒为 0，也还没有 tokenizer（账本按约 4 字符估算 token）。
- **`cargo clippy` 还不是 gate**：代码没有做过 lint sweep；加一个首次运行就会失败的 gate
  比没有更糟，它是格式化之后的下一步。
