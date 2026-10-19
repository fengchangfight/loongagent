# 失效模式与对策

Language: zh

- **Up:** [README.md](README.md)
- **Authority:** [ADR-0006](../decisions/0006-context-engine.md) · [ADR-0017](../decisions/0017-layered-context.md)
- **Code:** `crates/context/src/store.rs`, `crates/context/src/worker.rs`, `crates/context/src/summaries.rs`
- **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`

## 8. 失效模式与对策（本设计的核心风险表）

| # | 失效模式 | 后果 | 对策（机制，非约定） |
|---|---|---|---|
| 1 | **漂移**：L4 越抽象越偏离事实 | 模型自信地记错项目状态 | ①**禁止链式总结**：每层只从当前子层重派生，旧版摘要不进输入集合（[principles.md](principles.md) §4.2.1，`summary_inputs` 可机械检查）②引用区间 + `superseded_by`（**列在、从未写入**，见 [status.md](status.md) §11.1）+ `expand` 强制带原文（[recall.md](recall.md) §7.2 要点 1） |
| 2 | 高层摘要吞掉关键细节 | 决策依据消失 | digest 保底（[pipeline.md](pipeline.md) §6.2）+ L1 原子记录 + 关键词索引按需回填 |
| 3 | 摘要被当事实 | 错误结论在会话内传播 | 系统提示纪律 + `expand` 强制带原文 + 摘要标注 `producer` 与 `status` |
| 4 | 后台拖慢交互 | 体验变差 | 单飞行 + 窗口预算 + 前台优先 + 可关闭（[principles.md](principles.md) §4.6）；走本机 HTTP 端点时模型在别的进程里 |
| 5 | 归纳成本失控 | 电费/延迟爆炸 | 增量水位 + 脏集合 + 上行收敛 + 每窗口硬预算 |
| 6 | 小模型质量不足（编造决策 / 漏掉状态） | 归纳无用甚至有害 | **忠实度测试作为采纳门槛**（[rollout.md](rollout.md) §14 未决项 2）：10 份真实 digest + ground truth 计编造/漏项；过不了就升档或退回 R 模式 |
| 7 | 存储膨胀 | 磁盘增长 | **不建任何原文/摘要全文索引**（[data-model.md](data-model.md) §5.3、[rollout.md](rollout.md) §14 未决项 4）；`atom.text` 是抽取物且有上限；原文不复制；`vacuum`/保留策略 |
| 8 | 索引损坏/版本升级 | 记忆不可用 | 可丢弃可重建是硬要求；`schema_ver` + 重建路径有测试 |
| 9 | 多进程并发写同一 DB | 损坏 | 一个会话内**两个连接**（前台派生 + 后台富化）：WAL + `busy_timeout`（输者等赢家的短事务）；进程之间仍是"一个会话一个写者"，没有做"检测到其他写者则让位"。注意 WAL 下 checkpoint 前比较体积无意义 |
| 10 | 摘要掩盖了"被否决的方案" | 重蹈覆辙 | L2 synthesis 的**必填字段**：`rejected[]`、`open_questions[]` |
| 11 | 关键词查询静默返回 0 | 模型以为"没有这段历史" | 关键词逐词 AND（[recall.md](recall.md) §7.2 要点 5）；不走 FTS5，从根上避开短词陷阱 |
| 12 | **输入里没有的信息，模型只能编或弃权** | 要么记忆变薄，要么记忆变假 | digest 必须携带**尝试序列**（每次改动 + 紧随其后的验证结果，[pipeline.md](pipeline.md) §6.2）；能算的字段一律由程序算，绝不让模型"抄一份它读得到的清单" |
| 13 | 静默降级（模型连不上、schema 版本不对） | 用户以为记忆在跑，其实没有 | `memory_degraded` 记录原因（引擎级失败 + "N 段退回 digest"），TUI F2 面板与状态栏 `mem ⚠ degraded` 显示（**已实现 2026-10-14**，见 [status.md](status.md) §11.1） |
