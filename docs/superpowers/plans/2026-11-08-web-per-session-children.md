# Web surface：每 session 一个子进程（施工计划）

给执行 agent 看的。设计理由与取舍在
[design](../../design/web-surface.md)。本文是逐任务、TDD 的施工计划。

## 目标（一句话）

`loongagent web` 启动不持有任何 session；server 子进程按 **(workspace, session)** 键控，
同一 workspace 多个 session 各活各的、切换不杀；左边 rail 改成 **workspace → session**
两层树；workspace 目录列表持久化到 `workspaces.json`。

## owner 已拍板，不要重新讨论

1. **不动底层 server 的会话模型。** `loongagent server` 永远单 session、`wrong_session`
   守卫一行不动。多 session 并存靠"每 session 起一个 `--session <id>` 子进程"，不是靠
   让底层支持多 session。
2. **切换 session 不杀子进程。** 旧 session 的子进程留着，正在跑的 turn 不打断。
3. **workspace 持久化用一个小 json 文件**（配置目录下 `workspaces.json`)，不上 KV。
4. **web surface 不要"预选 session"。** `loongagent web` 不再接受 `--session`/`--new`
   去预建 session;launch 目录是唯一在启动时固定的东西。

## 基线状态（开工前必须确认仍是绿灯）

整个"多 workspace 版" web surface 是一批**未提交**的工作（tracked 修改 + 未跟踪新文件
`app.rs`/`app.js`/`sessions.rs`/`web-surface.md` 等），最后提交停在
`5b9cf2b web: auto-approve…`。本计划在这批未提交改动之上继续。

已验证基线健康：
```bash
cargo build -p loong-web -p loongagent   # 过
XDG_RUNTIME_DIR=$PWD/.scratch/rt TMPDIR=$PWD/.scratch cargo test -p loong-web   # 25 过
```
**注意**：这批改动未提交，工作区可能随时被别的会话改动。动手前先 `git status --short`
核对上面那批文件还在，再 `cargo test -p loong-web` 确认绿灯。若变红，先停下来问 owner,
不要在别人的半成品上叠改动。

## 当前架构要害（改动落点）

- `crates/cli/src/web.rs` `start_session` 闭包：**签名是 `Fn(&Path)`** —— 只按目录起一个
  子进程，且 `workspace_args.session_spec = None`（总是新 session)。这是"每 workspace
  一个子进程"的根源，要改成 **按 (dir, session_id) 起**。
- `crates/web/src/app.rs`:
  - `Workspace { dir, session: Option<…>, busy }` —— 一个 workspace 只挂**一个** session。
  - `start_session: &dyn Fn(&Path)` 参数一路传进 `handle_ws`/`dispatch_surface`/`forward`/`ensure_session`。
  - `session.open` 分支：守卫 `open` 目标后 `Ok(SurfaceCall::Forward)`,**透传**给子进程。
  - `ensure_session`:`workspaces[seat].session.is_none()` 时才起。
- `crates/web/src/sessions.rs`:`session_dir`/`list`/`known`/`delete`，按目录列 session。
- 前端 `crates/web/src/app.js` + `index.html`：现在是一层 session list，要改成两层树。

## 数据模型改动（核心）

把"每 workspace 一个 session"改成"每 (workspace, session) 一个子进程":

- `Workspace` 去掉单个 `session` 字段，改成 `children: HashMap<String, Arc<Mutex<dyn Session>>>`,
  键是 session id。`busy` 也要从 workspace 级下沉到 **每个 child 级**（否则同 workspace
  两个 session 会互相误判 busy)。
- tab 的"seat"现在是 `Option<usize>`(workspace index)，要扩成
  `seat: Option<usize>` + `attached: Option<String>`(session id)。
- `start_session` 闭包签名改成 `Fn(&Path, &str) -> Result<Arc<Mutex<dyn Session>>, String>`,
  第二个参数是 session id；内部 `workspace_args.session_spec = Some(session_id)`,
  让 `ChildTransport::spawn` 带 `--session <id>` 起。
  - **新建 session** 时传什么 id？设计文档 §2 说"spawn a server child for a fresh
    session… it answers session.attach with the real id"。这里有个待定点见下面"风险"。

## 任务分解（每个任务 TDD：先红后绿）

### Task 0 — 守基线
确认 `cargo test -p loong-web` 与 `cargo test -p loongagent --test web_surface` 全绿，
`./scripts/check.sh` 绿。红则停工上报。**不提交**任何东西（基线本身未提交）。

### Task 1 — `start_session` 按 (dir, session) 起子进程
- 改 `web.rs`:`start_session` 收 `(dir, session_id)`,`session_spec = Some(id)`。
- 同步改 `app.rs` 里所有 `&dyn Fn(&Path)` 类型与调用点。
- 测试：在 `web.rs` 层不好单测（要起真子进程），此任务以**编译通过 + 现有 web 测试仍绿**
  为门槛；行为验证留给 Task 3 的集成测试。

### Task 2 — `State`/`Workspace` 数据模型改成 children map
- `Workspace.session: Option` → `children: HashMap<String, Arc<Mutex<dyn Session>>>`;
  `busy` 下沉为 `HashMap<String, Option<u64>>` 或并入 child 条目。
- `handle_ws` 的 `seat` 扩成 `(workspace index, attached session id)`。
- 先写红的单测：`app.rs` 的 dispatch 测试里构造一个 state，断言同一 workspace 能挂两个
  不同 session id 的 child。
- 绿：改结构让测试过。**这一步会牵动大量现有测试签名**，预期一批编译错，逐个修。

### Task 3 — surface 接住 `session.open`/`session.new`，不再透传
- `session.open`：不再 `Forward`。改为：确认目标 session 存在 → 为该 (dir, session)
  `ensure_child`（没有就 `start_session(dir, id)`)→ 把 tab 的 `attached` 设成它 → 在
  socket 上回 attach 结果。
- `session.new`：同理，起一个新 child。
- 先写红的集成测试（`crates/cli/tests/web_surface.rs` 或 `app.rs` 的 ws 测试）:
  - 空手 tab `session.open {session_id:"", open:"s1"}` → 成功，**不再出现**
    `unknown session ""`。
  - 连续 open 两个 session，再回头给第一个发 `prompt.submit` → 两个 child 都在、各答各的。
- 绿：实现到测试过。

### Task 4 — workspace 持久化到 `workspaces.json`
- `State::new` 时从配置文件读 workspace 列表；`workspace.add`/`close` 时写回。
- 文件位置复用 `sessions::client_root()` 那套配置目录约定。
- 测试：加一个 workspace、重建 `State`、断言还在。注意测试间隔离（`LOONGAGENT_HOME`
  已有 `HOME_LOCK` 模式可仿）。

### Task 5 — 前端两层树 UI
- `app.js`:`session.list` 改成按 workspace 分组拉取（每个 workspace 一份 session list),
  渲染成 workspace → session 两层。
- `index.html` + `style.css`：树的结构样式。
- 无 JS 测试框架：以**手动/集成**验证为准，但至少保证 `session.list` 返回结构被
  前端正确消费（可在 `app.rs` 测试里断言返回 json 形状）。
- **此任务等 Task 1–4 绿了再做**，且尽量小步：先能渲染两层，再谈交互细节。

### Task 6 — 文档与门
- `web-surface.md` 已在 design 阶段更新；实现中若偏离，回写文档。
- `docs/crate-map.md`、`docs/README.md` 的测试计数若有变，更新。
- 收尾跑 `./scripts/check.sh` 全绿。

## 风险 / 待定点

1. **新建 session 的 id 哪来。** 设计说 surface 起 child 后由 child `session.attach`
   回真实 id。但 `start_session(dir, id)` 需要一个 id 才能 `--session <id>`。可能要先
   让 child 以"fresh"方式起（`session_spec=None` 落 default，或让 server 支持"起一个不
   绑定 session 的 child")。**这是实现时最可能卡住的点**，动手 Task 1/3 时先想清楚，
   必要时回 design 补一小节。
2. **`busy` 下沉后**,`forward` 里 `prompt.submit` 的 busy 标记要按 child 记，别让
   同 workspace 的 idle session 被误判 busy。
3. **测试隔离**:`LOONGAGENT_HOME` 是进程级环境变量，多测试并发会打架，沿用现有
   `HOME_LOCK` 串行化。
4. **不要在基线红灯上施工**，也不要把别人的未提交 hunk 一起 stage。

## 验证（收尾必跑）

```bash
XDG_RUNTIME_DIR=$PWD/.scratch/rt TMPDIR=$PWD/.scratch cargo test -p loong-web
XDG_RUNTIME_DIR=$PWD/.scratch/rt TMPDIR=$PWD/.scratch cargo test -p loongagent --test web_surface
./scripts/check.sh
```
全绿才算完。
