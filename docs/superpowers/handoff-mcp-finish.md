# 交接:loongagent MCP 支持功能 — 收尾任务

## 这是什么

仓库 `/home/fengchang/test/loongcoder` 里有一组**未提交**的改动(19 个文件修改 + 1 个新文件
`crates/tools/src/deferred.rs`,约 +1716/-283 行),实现 loongagent 的 MCP(Model Context
Protocol)客户端支持。设计文档在 `docs/design/mcp-client.md`(已提交/已更新),改动涉及的
文档还有 `docs/design/README.md`、`docs/design/extension-model.md`、`docs/design/security-trust.md`、
`docs/design/sessions-lineage.md`、`docs/topics/open.md`。

实现本身已经完成并通过测试,**剩下的只是收尾机械活**。下面的任务互相独立,可以并行做。

## 开始前必读

1. `AGENTS.md`(仓库根)— 契约规则:每个 `.rs` 文件要有 `//!` 模块契约,每个非平凡私有
   函数要五标签(Purpose/Serves/Design/Up/Down/Verify),`docs/human-written/` 绝对不可动。
2. `docs/design/mcp-client.md` — 功能的设计文档,是改动的依据。
3. 完成每个任务后跑 `bash scripts/check.sh`(注意:必须用 bash,不能用 python3),全绿才算完成。

## 当前状态(2026-XX 交接时实测)

- `cargo test -p loong-kernel`、`-p loong-mcp` 全部通过(含新增的 deferral 和 list_changed 测试)。
- `python3 scripts/check-docs.py` 通过。
- `python3 scripts/check-modules.py` 失败 1 处(见任务 A)。
- `bash scripts/check.sh` 里 `cargo fmt --check` 失败 2 处(见任务 B)。
- 有一个临时脚本 `fix_contracts.py` 在仓库根,用完应删除。

## 任务

### 任务 A:补 `expand_headers` 的契约块(5 分钟)

文件 `crates/mcp/src/config.rs` 第 394 行的私有函数 `expand_headers`(40 行)只有 Purpose+Contract
两标签,需要五标签。参照同文件 `check_url`(约 358 行)刚改好的格式,按函数实际行为填写
Serves/Design/Up/Down/Verify。函数作用:把 manifest 里的 header 表展开成 `(name, value)` 列表,
支持 `${ENV_VAR}` 占位符。Verify 指向 config 测试名(在文件里 grep `expand_headers` 的调用方测试)。

完成后:`python3 scripts/check-modules.py` 必须全绿。

### 任务 B:`cargo fmt`(2 分钟)

`bash scripts/check.sh` 报 `crates/mcp/src/lib.rs` 有一处 import 排序和一处单行化格式不符合
rustfmt。直接跑 `cargo fmt` 即可,不要手动改。

完成后:`cargo fmt --check` 必须干净。

### 任务 C:跑通全部门禁并提交(30 分钟)

1. 删掉根目录的临时脚本 `fix_contracts.py`(它是上个 agent 留下的工作脚本,不属于交付物)。
2. `bash scripts/check.sh` 全绿。
3. 全量回归:`cargo test --workspace`。
4. 按仓库提交习惯提交(先看 `git log --oneline -5` 学风格)。注意:`docs/human-written/`
   本次没有改动,提交**不要**带 `Human-Written: true` trailer。
5. 如果改动要拆成多个 commit,按 crate/主题拆;一个 commit 也可以,message 说明这是
   MCP 客户端支持(stdio+HTTP 传输、工具/提示词、deferral 与 tool_search、list_changed 刷新、
   审批门与 hook 集成)。

### 任务 D(可选,最后做):复核 `docs/topics/open.md` 的未决项

该文件列了 MCP 相关的 open question,检查本次实现是否已经回答了其中某些条目;已回答的
在条目里注明指向 `docs/design/mcp-client.md` 对应小节。不确定就不要动。

## 不要做的事

- 不要改 `docs/human-written/` 下任何文件。
- 不要重写实现逻辑;如果测试失败,先按 `superpowers:systematic-debugging` 流程查,不要猜。
- 不要在 commit 里加 `Human-Written: true` trailer。
