# loongagent for VS Code

这是 loongagent 的 VS Code 面板：编辑器区的一个 tab，从编辑器标题栏的 loongagent 图标或
`loongagent: Open Chat` 打开；不是 Copilot 的聊天框，也不是 Activity Bar 侧边栏。
它不需要 Copilot 登录，也不需要选 VS Code 的模型；模型来自 `loongagent server`。

## 打包并安装 VSIX

需要 Node.js 和 npm。在 `clients/vscode` 下执行：

```bash
cd clients/vscode
npm ci
npm run package:vsix
```

`package:vsix` 会先跑 `npm run compile`（esbuild 打包 + `tsc --noEmit` 类型检查），再用
`@vscode/vsce package` 生成 `loongagent-vscode-0.1.0.vsix`；文件名里的版本号取自
`package.json`。产物就在 `clients/vscode/` 目录下。

然后安装：

```bash
code --install-extension loongagent-vscode-0.1.0.vsix
```

VSIX 只包含扩展；先按下面说明安装 `loongagent` binary，扩展才能在启动时找到它。

## 卸载

```bash
code --list-extensions | grep -i loong            # 确认扩展 ID
code --uninstall-extension loongagent.loongagent-vscode
```

## 开发运行

```bash
cd clients/vscode
npm ci
npm run compile          # dist/extension.js + dist/webview.js，然后 tsc --noEmit
npm test                 # node --test，纯逻辑模块

mkdir -p /tmp/loong-vscode-scratch
code --extensionDevelopmentPath="$PWD" --new-window /tmp/loong-vscode-scratch
```

开发宿主窗口里点编辑器标题栏的 loongagent 图标，或命令面板执行 `loongagent: Open Chat`。
面板会开在文件旁边（独立编辑器分组），所以文件和聊天可以同时看到；第一次点击打开面板；面板已经打开时，每次再点图标都会新建一个会话 tab。没有打开编辑器时，
标题栏没有这个图标，用命令面板即可。F5 需要本地 `.vscode/launch.json`
（仓库不提交 `.vscode/`），命令行方式更省事。

## 找到 loongagent binary

扩展不打包 Rust binary。解析顺序：

1. `loongagent.path`（machine scope）
2. **loongagent: Locate the binary** 选过的路径
3. PATH 里的 `loongagent`
4. `~/.cargo/bin`、`~/.local/bin`、`/usr/local/bin`、`/opt/homebrew/bin`

如果从桌面图标启动 VS Code，PATH 可能没有 `~/.cargo/bin`，这时在设置里写绝对路径：

```json
"loongagent.path": "/home/you/.cargo/bin/loongagent"
```

或者命令面板执行 **loongagent: Locate the binary**。输出面板 `loongagent` 会显示实际路径、
版本和 schema。

server 默认走 local（`~/.loongagent/local.toml` + `~/.loongagent/env`）。如果要读企业下发的
profile/control pack，在用户设置里加：

```json
"loongagent.extraArgs": ["--centered"]
```

## 写入前审阅

要让 `write_file` / `edit` 在落盘前弹出 diff 卡片，在策略里写：

```toml
[permissions]
write_workspace = "ask"
```

扩展不修改策略。如果策略是 `allow`，写入会直接发生，聊天里会提示“written unreviewed”，
之后仍可用 Review 卡片查看 diff。

## 当前功能

- 流式回答、工具卡片、Stop；等第一个 token 时显示带转圈的 `Thinking…`
- 模型下拉（来自 `model.list` / `model.select`）
- `@ file` 附件：输入 `@`，输入框上方出现文件列表，直接输入前缀过滤；选中后输入框保留 `@相对路径`，文字不会被删掉
- 编辑器里选中代码后自动带上，不需要 selection 按钮；点回聊天后仍保留，新 tab 也继承
- 没有选中代码时自动带上当前聚焦的文件和光标行号（`current file: … (cursor at line N)`），面板上显示 `file · 路径:行号`
- 发送后手动 `@ file` 清空；编辑器选区还在就保留，下一条继续带
- 写入前 diff 卡片：Allow once / Allow always / Deny；Open full diff
- 一轮结束后的 Review 卡片：逐个打开 `vscode.diff`，左边是该轮第一次提到文件时的内容
- `permissions.relaxed` 显示
- `/new`、`/model`、`/context` 命令；`/yolo`、`/god`、`/ask` 切换提问强度，和 TUI 同一套语义
- 顶部模式徽标：`ask` 时不显眼，`⚠ YOLO` 和 `⚡ GOD` 各自标出来；点一下按 `ask → yolo → god → ask` 走。切换是一步到位，不弹确认框（TUI 也是一个键切过去）
- 切换不阻塞：turn 在跑的时候点，徽标立刻变（虚线，标明「本轮结束后生效」），这一轮跑完自动落地。server 在 turn 期间根本不会读取新请求，所以等它就是干等
- 每次切换在记录里写一句该模式自己的承诺：`YOLO` 不动沙箱、不可逆动作照样问；`GOD` 不问删除和外发，但越界删除和密钥外发直接拒绝
- 审批卡只在服务端还握着那个请求时能点；上游进程没了的卡片会标成 `expired` 并说明原因，不会点了没反应
- 服务端说 `approval … is still pending` 时自动否掉那条并重发一次，不会再卡成发不出消息
- 多 tab：一个 tab 一个会话、一个子进程；最多 5 个；同一 session 只开一个
- 关闭 tab 即拒绝 pending approval 并 dispose 子进程；`History` 打开最近会话
- 重载后恢复 tab、活动 tab、会话和最近消息

## 限制

- 不做逐 hunk 接受/拒绝；只做整个文件的 diff。
- `shell` 的写入没有预览，只有策略/审批卡片。
- Windows 上 Stop 未实现（POSIX `SIGINT`）；停在审批上的那一轮，Stop 用「拒绝该请求」收尾，不发信号。
- 一个窗口最多 5 个会话；跨窗口的 write token 不在本阶段。
- 原生 `@loong` participant 已移除；VS Code 入口就是这个编辑器面板。

设计与实现计划：

- [docs/design/vscode-surface.md](https://github.com/fengchangfight/loongagent/blob/main/docs/design/vscode-surface.md)
- [M1 计划](https://github.com/fengchangfight/loongagent/blob/main/docs/superpowers/plans/2026-10-26-vscode-webview-m1.md)
- [M2 计划](https://github.com/fengchangfight/loongagent/blob/main/docs/superpowers/plans/2026-10-26-vscode-webview-m2.md)
