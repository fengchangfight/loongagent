# 前台合成与 `recall`

Language: zh

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0006](../decisions/0006-context-engine.md) · [ADR-0017](../decisions/0017-layered-context.md)
- **Code:** `crates/kernel/src/turn/recall.rs`, `crates/context/src/store.rs`
- **Verify:** `cargo test -p loong-context -p loong-kernel` · `./scripts/check.sh`

## 7. 前台合成与 `recall`

### 7.1 前台怎么变

| 分类 | 今天 | 改造后 |
|---|---|---|
| system / tools / rules / skills | 不变 | 不变 |
| L4 地图 | 不存在 | 新 `Category::CognitiveMap`（`Lifetime::Session`, `pinned`），按 `replace(id, text)` 原地更新 |
| 历史消息 | 累积到 ceiling → 清理/丢弃 | R 模式同今天；S 模式**只清工具输出**，不丢消息 |
| recall 结果 | 不存在 | **不另记账本条目**：答案作为普通 `tool.result` 进入历史，由 history 分类计费（原因见 §7.2 要点 8） |

`Category::Retrieval` 这个分类**存在于枚举里但 `recall` 不使用它** —— 早期设计想让检索
结果以 turn 生命期参与预算竞争，实现时发现那会把同一批字节**双重计入**上下文预算。
`Lifetime::Turn` 的账本条目目前仍被预算淘汰逻辑使用，只是不由 `recall` 产生。

L4 必须**有 token 上限**：当前是常量 `L4_MAX_TOKENS = 400`（`synthesis.rs`），
`[context_engine] l4_max_tokens` 虽被解析但**还没有被读取**。R 模式与 S 模式
共用同一个确定性 L4；草案里"R 模式的 L4 更短（200）"没有实现。

### 7.2 `recall` 工具（唯一检索入口）—— 已实现

```jsonc
// 调用 1：定位（摘要优先，摘要为空才回落到原文事件）
{"query": "层叠 配置", "entity": "src/config", "tool": "edit_file", "level": 2, "limit": 8}
→ memory (summaries; expand one before relying on it):
  - [ep-00000118-00000143] L2 修复 config 层叠失效 (done) seq[118-143]
    L2 · … | 状态：done | 改动文件：src/config/merge.rs | …

// 调用 2：展开（摘要正文 + 它引用的原始事件，一次到位）
{"expand": "ep-00000118-00000143"}
→ summary [ep-00000118-00000143] L2 …（正文）
  evidence (the original events this summary rests on):
  [121] tool.result FAILED run_command: test layer_merge ... FAILED
```

设计要点（实现后的版本）：

1. **`expand` 强制同时给原文**，不只给摘要正文 —— 从机制上防止"把摘要当真相"。
2. `expand` 是**纯读**（摘要已物化），不触发模型，因此不引入延迟与抖动。
3. 被清掉的工具输出仍可从日志取回（evidence 读 `atom`，读的是抽取物与 `seq`）。
4. `limit` 默认 8、封顶 32；整条答案有**硬字符上限**（`RECALL_MAX_CHARS = 4 800`），
   截断时显式标注 —— 一个被悄悄截短的检索结果会读成"这就是全部"。
5. 关键词**逐词 AND**，每个词独立匹配（不走 FTS5，从根上避开 `<3` 字静默返回 0）。
6. **搜不到是一个正常答案，不是错误**：返回"没有匹配，但别据此断定什么都没发生"。
   返回错误会教会模型"问过去的事是坏的"。
7. `recall` **绕过 PDP 且不产生审批**：它只读本次会话自己的记录，而这份记录模型
   已经完整拥有过；没有可以门禁的东西。调用本身仍然是一条 `tool.call` 事件。
8. `recall` **不额外记一条 `Category::Retrieval` 账本条目**：答案会变成 tool-result
   消息，账本已经在计这些 token；再记一条会让同一批字节被双重计入上下文预算，
   而那是人用来判断"这个会话花不花得起"的那个数字。
9. `recall` **只在引擎开启时出现在工具表里**：模型能调但答不出的工具，比没有这个
   工具更糟 —— 它会教会模型"问过去没用"。

### 7.3 系统提示的纪律（一小段，不是长篇）

> 关于过去的工作，先 `recall` 再回答；任何"已验证/已修复"的断言都必须带 evidence 区间。
> 摘要是派生索引，可能与原文不符 —— 有疑问时展开原文，以原文为准。
