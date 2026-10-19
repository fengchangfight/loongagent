# 06 · Context engineering — 分层可逆记忆

Language: zh

- **Up:** [context/README.md](context/README.md)
- **Authority:** [ADR-0006](decisions/0006-context-engine.md) · [ADR-0017](decisions/0017-layered-context.md)
- **Code:** `crates/context/src/lib.rs`, `crates/kernel/src/ledger.rs`, `crates/kernel/src/turn/recall.rs`
- **Verify:** `cargo test -p loong-context -p loong-kernel` · `./scripts/check.sh`

本文是上下文设计的入口。核心主张是：把上下文管理从**有损压缩**改成**可逆的信息组织** ——
原文永不离开事件日志，前台只携带少量"认知摘要"，需要时用关键词索引一层层定位回原始证据。
完整的 L1–L4 结构、两种运行模式、各组机制细节与当前限制都移到了
[`docs/context/`](context/README.md)；旧文件名保留，外部链接照常可用。下面的表是每一节的落点。

## 章节去向

| 原章节 | 现在的位置 |
|---|---|
| §0 一句话、§1 现状、§2 目标结构、§3 两种运行模式、§10 与现有 ADR 的关系 | [context/README.md](context/README.md) |
| §4 原则（原文是唯一真相、上行收敛、不链式总结、只处理一次、先确定性后模型、稳态、前台优先） | [context/principles.md](context/principles.md) |
| §5 数据模型（位置、DDL、`atom.text`、实体抽取） | [context/data-model.md](context/data-model.md) |
| §6 流水线（统一形状、digest/synthesis、切段、上行触发、走一遍） | [context/pipeline.md](context/pipeline.md) |
| §9 事件种类（后台工作与摘要变更的可见事件） | [context/pipeline.md](context/pipeline.md) |
| §7 前台合成与 `recall`（唯一检索入口、系统提示纪律） | [context/recall.md](context/recall.md) |
| §8 失效模式与对策（核心风险表） | [context/failure-modes.md](context/failure-modes.md) |
| §11 配置、§11.0 `@` 文件引用、§11.2 模型不写文件 | [context/config.md](context/config.md) |
| §11.1 缺口清单与已落地（当前状态与限制） | [context/status.md](context/status.md) |
| §12 非目标、§13 分阶段实现、§14 待讨论 | [context/rollout.md](context/rollout.md) |
