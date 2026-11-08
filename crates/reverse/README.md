# `crates/reverse` — the vendored reverse-engineering surface

Up: [docs/design/reverse.md](../../docs/design/reverse.md)
Authority: [ADR-0009](../../docs/decisions/0009-surfaces.md)
Code: `crates/reverse/vendor/`（`loong-reverse-{evidence,platform,formats,disasm,js,analysis,engine,mcp,testkit}`）
Verify: `scripts/check-reverse.sh`（`scripts/check.sh` 会跑它）

这些 crate 是 Craft Anything 的源码副本，不是 cargo/git 依赖：本仓库自己编译它们，产物进
`loongagent-reverse` 二进制。许可证与逐项改动见 [vendor/ATTRIBUTION.md](vendor/ATTRIBUTION.md)。

## 来源

| 项 | 值 |
|---|---|
| 上游仓库 | `/home/fengchang/test/craft-anything`（https://github.com/storytold/craft-anything） |
| 上游 commit | `fc93bd0`（2026-11-05） |
| 上游包名 → 本仓库包名 | `craft-anything-X` → `loong-reverse-X` |
| 标识符改名 | `craft_anything_*` → `loong_reverse_*` |

## 同步（重新 vendor 一遍）

重新 vendor 是「复制 + 下面这份改动清单」，不引入依赖、不改上游 API：

1. 复制上游 `crates/{evidence,platform,formats,disasm,js,analysis,engine,mcp,testkit}` 到
   `crates/reverse/vendor/`，包名与标识符按上表改名；
2. 删掉 `craft-anything-engines` 依赖，把 `cdp.rs` 放进 `loong-reverse-engine/src/cdp.rs`，
   外部程序查找用 `loong-reverse-engine/src/locate.rs`；
3. 不复制 Ghidra / JEB / Frida / rizin-cutter-gdb / objdump / 游戏资源 / Electron / wasm-plugin
   这些桥：随桥一起删掉它们的模块（`android_tools/jeb.rs`、`deep.rs` 里的引擎分支、
   `platform` 的 PTY 与 serve 模块等），`tools/mod.rs` 不再列举它们的 spec；
4. Clean room / Deep 模式取消：`session::Session::clean_room()` 恒为 `false`，
   `clean_room.rs` 只留形状（`allows`、`availability`、`refusal`、`is_refusal`、`mode_words`）；
5. `cargo fmt` 到本仓库风格，crate 根文件写 `//!` 契约头（`Purpose:` 开头，随后是
   `> Vendored from …` 出处行），其余文件保留上游注释；
6. `Cargo.toml` 根用显式 `members` 列出每个 vendored 包（`crates/*` 通配匹配不到嵌套目录）；
7. `.gitignore` 的 `vendor/` 规则之外放行 `crates/reverse/vendor/`——这段源码要进仓库；
8. 跑 `scripts/check-reverse.sh` 与 `cargo test -p loong-reverse-engine`；工具清单变化时
   `tools/mod.rs` 里 `pinned_surface::PINNED` 与 `tool_list::MIN_TOOLS` 同步更新。

vendored 目录整体在 `scripts/contract-exemptions.txt` 里作为「目录豁免类」登记：逐 item 的
`Purpose:`/`Serves:` 契约留在上游维护，重新 vendor 才不会变成手工重写 8 千个文档块。
