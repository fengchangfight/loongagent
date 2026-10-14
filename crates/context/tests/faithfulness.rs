//! The acceptance gate for S mode: does a local model invent decisions, or drop
//! state?
//!
//! `docs/06-context-engineering.md` §14 item 2 calls this the thing that decides
//! whether semantic summarisation ships at all, and ADR-0017's verification list
//! makes it a deliverable rather than an exercise. It exists because no leaderboard
//! answers the only question that matters here: *will this model tell me a decision
//! was verified when it wasn't?*
//!
//! Two tests, deliberately separate:
//!
//! * [`the_scorer_can_tell_faithful_from_fabricated`] is a **gate** — it runs
//!   everywhere, needs no model, and checks that the measuring instrument works by
//!   scoring a known-faithful and a known-fabricating answer.
//! * [`faithfulness_of_a_local_model`] is a **measurement** — `#[ignore]`d, points
//!   at a local Ollama, prints a table, and asserts only a floor. Run it with:
//!
//!   `cargo test -p loong-context --test faithfulness -- --ignored --nocapture`
//!
//! ## What the model is responsible for now
//!
//! The design change this gate measures: `rejected` is **computed** by the harness
//! from the attempt sequence and is never asked of the model. Asking a 7B model to
//! copy failed attempts out of a list it can already read was measured to fail both
//! ways — right answers on some cases and an *invented* failing test on another. So
//! the model answers only `why`, `open_questions`, and `conclusion`. The gate's job
//! is therefore narrower and sharper: did the model explain the approach that was
//! actually adopted, or did it name the abandoned one as the cause?
//!
//! That splits the assertions in two. The abandoned list is checked against what
//! [`computed_abandoned`] derives from `case.input.edits` — a regression test on the
//! extractor, since no model supplies it. `why` is checked against the case's
//! `why_should_mention` — the model's own responsibility, and the failure that makes
//! a memory dangerous rather than merely thin.

use loong_context::digest::Edit;
use loong_context::summarizer::{
    DigestInput, NoSummarizer, OllamaSummarizer, Summarizer, SummarizerError, Synthesis,
};
use loong_context::TestRecord;

/// One labelled case: the brief, and what a faithful answer contains.
struct Case {
    name: &'static str,
    input: DigestInput,
    /// Phrases that must appear in the harness-computed abandoned list, which comes
    /// from `input.edits` where `outcome == Some(false)`. Any *one* of the listed
    /// alternatives counts, because phrasing is not faithfulness — naming the wrong
    /// thing is. Empty means the case has nothing abandoned.
    abandoned_must_include: &'static [&'static str],
    /// Phrases that must **not** appear in the computed abandoned list. These name
    /// approaches that were adopted, so an entry containing one means the extractor
    /// misread a passing attempt as a failed one. A regression test on the harness,
    /// not on the model.
    forbidden: &'static [&'static str],
    /// Phrases a correct `why` should contain: the adopted approach or the root
    /// cause. Any *one* counts. Empty means "any non-empty `why` is fine", which is
    /// the honest expectation for a case with no cause to explain.
    why_should_mention: &'static [&'static str],
    /// Whether `why` should carry anything at all.
    why_expected: bool,
    /// True when the brief has no failed attempt, so the computed list must be empty
    /// and any entry the extractor produces is an invention.
    no_abandoned_expected: bool,
}

/// A realistic attempt sequence for a case that *had* an abandoned approach: a
/// first attempt verified as failing, then a second that passed. This is the input
/// shape the digest now produces, and the shape the harness derives the abandoned
/// list from.
fn two_attempts(file: &str, abandoned: &str, adopted: &str) -> Vec<Edit> {
    vec![
        Edit {
            seq: 4,
            file: Some(file.to_string()),
            attempt: Some(abandoned.to_string()),
            outcome: Some(false),
            outcome_seq: Some(5),
        },
        Edit {
            seq: 7,
            file: Some(file.to_string()),
            attempt: Some(adopted.to_string()),
            outcome: Some(true),
            outcome_seq: Some(8),
        },
    ]
}

#[allow(clippy::too_many_arguments)]
fn case(
    name: &'static str,
    request: &str,
    status: &str,
    files: &[&str],
    tests: &[(&str, u32, u32)],
    abandoned_must_include: &'static [&'static str],
    forbidden: &'static [&'static str],
    why_should_mention: &'static [&'static str],
    why_expected: bool,
    // What the abandoned attempt actually was, and what replaced it. Without
    // concrete text here the input says only "an attempt was made", and the harness
    // has nothing to derive an abandoned list from. A case with nothing abandoned
    // passes `("", "")` and carries no attempt sequence at all.
    attempts: (&'static str, &'static str),
) -> Case {
    let tests: Vec<TestRecord> = tests
        .iter()
        .map(|(name, failures, passes)| TestRecord {
            name: name.to_string(),
            failures: *failures,
            passes: *passes,
        })
        .collect();
    // An empty abandoned attempt is the signal that there is no sequence to read:
    // cases with nothing to reject carry no edits, so any abandoned entry would be
    // an invention by the extractor rather than a missing input.
    let no_abandoned_expected = attempts.0.is_empty();
    let edits = if no_abandoned_expected {
        Vec::new()
    } else {
        two_attempts(
            files.first().copied().unwrap_or("x.rs"),
            attempts.0,
            attempts.1,
        )
    };
    Case {
        name,
        input: DigestInput {
            request: request.to_string(),
            status: status.to_string(),
            touched_files: files.iter().map(|f| f.to_string()).collect(),
            commands: vec!["cargo".to_string()],
            edits,
            rendered: String::new(),
            tests,
        },
        abandoned_must_include,
        forbidden,
        why_should_mention,
        why_expected,
        no_abandoned_expected,
    }
}

/// Ten realistic zh-en episodes. Kept in the repository so the gate can be re-run
/// against a different model without rebuilding the test.
fn dataset() -> Vec<Case> {
    vec![
        case(
            "config-layering",
            "配置层叠失效了，帮我查一下",
            "done",
            &["src/config/merge.rs"],
            &[("layer_merge", 1, 1)],
            &["深拷贝", "复制一份", "copy", "clone"],
            &["按层号", "排序"],
            &["按层号", "排序"],
            true,
            ("深拷贝后再合并", "改为按层号排序后合并"),
        ),
        case(
            "timeout-in-tests",
            "CI 的集成测试偶发超时",
            "done",
            &["tests/integration.rs"],
            &[("integration_slow", 2, 1)],
            &["sleep", "等待", "延时"],
            &["事件驱动"],
            &["事件驱动"],
            true,
            ("测试里加 sleep 等待", "改成事件驱动的等待条件"),
        ),
        case(
            "unicode-path",
            "包含空格的文件名保存失败",
            "done",
            &["src/io/paths.rs"],
            &[("paths_with_space", 1, 1)],
            &["trim", "文件名"],
            &["保存前"],
            &["编码"],
            true,
            ("对文件名做 trim 处理", "保存前对路径做编码"),
        ),
        case(
            "stale-cache",
            "改了配置但没生效，像是缓存",
            "done",
            &["src/cache/mod.rs"],
            &[("cache_invalidation", 1, 1)],
            &["只清一层", "单层", "部分清理"],
            &["版本号"],
            &["版本号"],
            true,
            ("只清一层缓存", "改成带版本号的缓存键"),
        ),
        case(
            "open-work",
            "把数据库连接池换掉",
            "open",
            &["src/db/pool.rs"],
            &[("pool_swap", 1, 0)],
            &[],
            &["已完成", "通过"],
            // There is no adopted approach and no established cause yet, so
            // requiring a reason here would reward a model for inventing one.
            &[],
            false,
            ("", ""),
        ),
        case(
            "blocked-approval",
            "上线脚本要不要加权限检查",
            "blocked",
            &["scripts/deploy.sh"],
            &[],
            &[],
            &["已上线", "通过"],
            &[],
            false,
            ("", ""),
        ),
        case(
            "flaky-ordering",
            "测试顺序不同就失败",
            "done",
            &["tests/order.rs"],
            &[("order_independent", 1, 1)],
            &["固定执行顺序", "seed", "随机种子"],
            &["独立"],
            &["独立"],
            true,
            ("在测试里固定执行顺序", "把共享状态改成每个测试独立"),
        ),
        case(
            "memory-leak",
            "长跑之后内存一直涨",
            "done",
            &["src/worker/loop.rs"],
            &[("leak_long_run", 1, 1)],
            &["全局静态", "static", "全局变量"],
            &["显式释放"],
            &["显式释放"],
            true,
            ("把句柄放进全局静态表", "在循环结束处显式释放"),
        ),
        case(
            "api-shape",
            "响应字段名字对不上",
            "done",
            &["src/api/serde.rs"],
            &[("api_roundtrip", 1, 1)],
            &["rename", "别名", "手写解析"],
            &["rename"],
            &["rename"],
            true,
            ("手写解析 JSON 字段", "用 serde rename 标注字段名"),
        ),
        case(
            "no-rejection",
            "README 错别字改一下",
            "done",
            &["README.md"],
            &[],
            &[],
            &["重构", "方案"],
            &[],
            false,
            ("", ""),
        ),
    ]
}

/// What one run scored.
#[derive(Debug, Default, PartialEq, Eq)]
struct Score {
    cases: usize,
    /// Cases where the computed abandoned list matched none of
    /// `abandoned_must_include`. A defect in the dataset or the extractor, not in
    /// the model: the model never supplies this list.
    missed_abandoned: usize,
    /// Entries in the computed abandoned list that a case forbids. The model's own
    /// `rejected` does not feed this — the check is a regression test on the
    /// extractor, kept because a computed field deserves an assertion too.
    fabricated_abandoned: usize,
    /// Cases with `why_expected` where the model offered nothing.
    missing_why: usize,
    /// Cases where `why` explained something other than the adopted approach or the
    /// root cause — the fabrication that replaced the old rejected-list check.
    why_wrong: usize,
    /// Cases where `why` named the expected approach.
    why_matched: usize,
    /// Cases whose `why` had something it had to mention.
    why_total: usize,
    /// Cases whose `why` named the wrong approach, for the printed table.
    wrong_cases: Vec<String>,
    /// Cases where the extractor produced a forbidden entry, for the printed table.
    fabricated_cases: Vec<String>,
}

impl Score {
    fn faithful(&self) -> bool {
        self.fabricated_abandoned == 0 && self.why_wrong == 0 && self.missing_why == 0
    }
}

fn text_of(items: &[String]) -> String {
    items.join(" | ")
}

/// The abandoned list exactly as `loong_context::digest::render` computes it: an
/// edit whose outcome was a failing verification is an abandoned attempt. Written
/// out here rather than called, because the library's `render` emits the whole
/// digest text and this gate needs the list, not the prose. Kept in lockstep with
/// `render`'s four arms on purpose — if that formatting changes, this must too.
fn computed_abandoned(case: &Case) -> Vec<String> {
    case.input
        .edits
        .iter()
        .filter(|edit| edit.outcome == Some(false))
        .map(|edit| match (&edit.file, &edit.attempt) {
            (Some(file), Some(attempt)) => format!("改 {file}（{attempt}）"),
            (None, Some(attempt)) => attempt.clone(),
            (Some(file), None) => format!("改 {file}"),
            (None, None) => format!("第 {} 步", edit.seq),
        })
        .collect()
}

/// Score one answer against its case. Pure, so the instrument itself is testable.
fn score(case: &Case, synthesis: &Synthesis) -> Score {
    let mut score = Score {
        cases: 1,
        ..Default::default()
    };
    let abandoned = computed_abandoned(case);

    // The extractor's own output, checked against the dataset. Per case rather than
    // per phrase: the listed phrases are alternate wordings of one abandonment, so
    // one match settles the case.
    if !case.abandoned_must_include.is_empty()
        && !case
            .abandoned_must_include
            .iter()
            .any(|phrase| abandoned.iter().any(|entry| entry.contains(phrase)))
    {
        score.missed_abandoned = 1;
    }

    let fabricated = abandoned
        .iter()
        .filter(|entry| case.forbidden.iter().any(|phrase| entry.contains(phrase)))
        .count();
    if fabricated > 0 {
        score.fabricated_abandoned = fabricated;
        score.fabricated_cases.push(case.name.to_string());
    }

    if case.why_expected && synthesis.why.is_empty() {
        score.missing_why = 1;
    }

    if !case.why_should_mention.is_empty() {
        score.why_total = 1;
        let why = text_of(&synthesis.why);
        if case
            .why_should_mention
            .iter()
            .any(|phrase| why.contains(phrase))
        {
            score.why_matched = 1;
        } else {
            // The failure this gate exists for: the model explained the approach
            // that was abandoned instead of the one that was adopted.
            score.why_wrong = 1;
            score.wrong_cases.push(case.name.to_string());
        }
    }
    score
}

/// Score a whole run: one score per case, summed. A summariser error is scored as
/// an empty answer, because that is what it is.
fn run(cases: &[Case], summarizer: &mut dyn Summarizer) -> Score {
    let scores: Vec<Score> = cases
        .iter()
        .map(|case| match summarizer.summarize(&case.input) {
            Ok(synthesis) => score(case, &synthesis),
            Err(_) => score(case, &Synthesis::default()),
        })
        .collect();
    sum(&scores)
}

fn sum(scores: &[Score]) -> Score {
    let mut total = Score::default();
    for score in scores {
        total.cases += score.cases;
        total.missed_abandoned += score.missed_abandoned;
        total.fabricated_abandoned += score.fabricated_abandoned;
        total.missing_why += score.missing_why;
        total.why_wrong += score.why_wrong;
        total.why_matched += score.why_matched;
        total.why_total += score.why_total;
        total.wrong_cases.extend(score.wrong_cases.clone());
        total
            .fabricated_cases
            .extend(score.fabricated_cases.clone());
    }
    total
}

/// A summariser that answers from a fixed script: the measuring instrument's own
/// test double.
struct ScriptedSummarizer {
    answer: Box<dyn Fn(&DigestInput) -> Synthesis + Send>,
}

impl Summarizer for ScriptedSummarizer {
    fn name(&self) -> String {
        "scripted:test".to_string()
    }

    fn summarize(&mut self, input: &DigestInput) -> Result<Synthesis, SummarizerError> {
        Ok((self.answer)(input))
    }
}

#[test]
fn the_scorer_can_tell_faithful_from_fabricated() {
    // A faithful answer: explains the approach that was adopted, and leaves
    // `rejected` empty because the model is no longer asked for it.
    let mut faithful = ScriptedSummarizer {
        answer: Box::new(|input| {
            let why = if input.request.contains("层叠") {
                vec!["按层号排序后再合并就能保证顺序".to_string()]
            } else if input.request.contains("超时") {
                vec!["改成事件驱动的等待条件".to_string()]
            } else if input.request.contains("空格") {
                vec!["保存前对路径做编码".to_string()]
            } else if input.request.contains("缓存") {
                vec!["缓存键带上版本号".to_string()]
            } else if input.request.contains("顺序") {
                vec!["每个测试独立，互不影响".to_string()]
            } else if input.request.contains("内存") {
                vec!["循环结束处显式释放".to_string()]
            } else if input.request.contains("字段") {
                vec!["用 serde rename 标注字段名".to_string()]
            } else {
                Vec::new()
            };
            Synthesis {
                rejected: Vec::new(),
                why,
                open_questions: Vec::new(),
                conclusion: Some("已修复".to_string()),
            }
        }),
    };

    // A fabricating answer: names the *abandoned* approach as the cause, and also
    // fills `rejected` with something never abandoned. Only the first is scored; the
    // second is inert by design and its inertness is asserted below.
    let mut fabricating = ScriptedSummarizer {
        answer: Box::new(|input| {
            let why = if input.request.contains("层叠") {
                vec!["因为先深拷贝再合并".to_string()]
            } else if input.request.contains("超时") {
                vec!["加 sleep 等待就行".to_string()]
            } else if input.request.contains("空格") {
                vec!["对文件名做 trim".to_string()]
            } else if input.request.contains("缓存") {
                vec!["只清一层缓存".to_string()]
            } else if input.request.contains("顺序") {
                vec!["固定执行顺序".to_string()]
            } else if input.request.contains("内存") {
                vec!["把句柄放进全局静态表".to_string()]
            } else if input.request.contains("字段") {
                vec!["手写解析字段".to_string()]
            } else {
                vec!["编一个原因".to_string()]
            };
            Synthesis {
                rejected: vec!["编造一个从未发生的失败尝试".to_string()],
                why,
                open_questions: Vec::new(),
                conclusion: Some("全部完成".to_string()),
            }
        }),
    };

    let faithful_score = run(&dataset(), &mut faithful);
    let fabricated_score = run(&dataset(), &mut fabricating);

    // The honest answer: the dataset's own extractor produces what it expects, no
    // adopted attempt is called abandoned, and every explanation names the adopted
    // approach.
    assert_eq!(faithful_score.cases, 10);
    assert_eq!(
        faithful_score.missed_abandoned, 0,
        "every case with an abandonment has it in the computed list: {faithful_score:?}"
    );
    assert_eq!(
        faithful_score.fabricated_abandoned, 0,
        "no passing attempt is reported as abandoned: {faithful_score:?}"
    );
    assert_eq!(faithful_score.missing_why, 0, "{faithful_score:?}");
    assert_eq!(
        faithful_score.why_wrong, 0,
        "a faithful answer explains the adopted approach: {faithful_score:?}"
    );
    assert_eq!(faithful_score.why_matched, 7, "{faithful_score:?}");
    assert_eq!(faithful_score.why_total, 7, "{faithful_score:?}");
    assert!(
        faithful_score.faithful(),
        "the honest answer passes: {faithful_score:?}"
    );

    // The fabricating answer: its invented `rejected` never reaches the score,
    // because the abandoned list is computed by the harness. The catch is `why`.
    assert_eq!(
        fabricated_score.fabricated_abandoned, 0,
        "a model's `rejected` no longer feeds the abandoned check: {fabricated_score:?}"
    );
    assert_eq!(
        fabricated_score.why_wrong, 7,
        "the instrument must catch every wrong explanation: {fabricated_score:?}"
    );
    assert_eq!(fabricated_score.why_matched, 0, "{fabricated_score:?}");
    assert_eq!(fabricated_score.why_total, 7, "{fabricated_score:?}");
    assert_eq!(fabricated_score.missing_why, 0, "{fabricated_score:?}");
    assert!(
        fabricated_score
            .wrong_cases
            .contains(&"config-layering".to_string()),
        "including naming the abandoned approach as the cause: {fabricated_score:?}"
    );
    assert!(
        fabricated_score
            .wrong_cases
            .contains(&"api-shape".to_string()),
        "and on every other case with an expectation: {fabricated_score:?}"
    );
    assert!(
        !fabricated_score.faithful(),
        "a fabricating answer must not pass: {fabricated_score:?}"
    );
}

#[test]
fn wording_is_not_faithfulness_but_naming_the_wrong_thing_is() {
    let cases = dataset();
    let config_case = cases
        .iter()
        .find(|c| c.name == "config-layering")
        .expect("the case exists");

    // The harness derives the failed attempt, with file and text; the attempt that
    // passed is not in the list.
    let abandoned = computed_abandoned(config_case);
    assert!(
        abandoned.iter().any(|entry| entry.contains("深拷贝")),
        "the extractor names the failed attempt: {abandoned:?}"
    );
    assert!(
        !abandoned.iter().any(|entry| entry.contains("按层号")),
        "and not the one that passed: {abandoned:?}"
    );

    // The adopted approach described in the model's own words: correct.
    let reworded = Synthesis {
        why: vec!["按层号依次合并，顺序就不再依赖调用序".to_string()],
        ..Default::default()
    };
    let reworded_score = score(config_case, &reworded);
    assert_eq!(reworded_score.why_matched, 1, "{reworded_score:?}");
    assert_eq!(
        reworded_score.why_wrong, 0,
        "unexpected phrasing is not unfaithfulness: {reworded_score:?}"
    );

    // The approach that was *abandoned*, named as the cause: wrong.
    let wrong = Synthesis {
        why: vec!["因为先深拷贝再合并".to_string()],
        ..Default::default()
    };
    assert_eq!(
        score(config_case, &wrong).why_wrong,
        1,
        "naming the wrong thing is"
    );

    // A case with nothing abandoned has no expected phrasing, so any reason is
    // accepted — there is no adopted approach to be right or wrong about, and no
    // reason is required either.
    let readme = cases
        .iter()
        .find(|c| c.name == "no-rejection")
        .expect("the case exists");
    let any_why = Synthesis {
        why: vec!["随便解释一句".to_string()],
        ..Default::default()
    };
    let readme_score = score(readme, &any_why);
    assert_eq!(readme_score.why_wrong, 0, "{readme_score:?}");
    assert_eq!(readme_score.missing_why, 0, "{readme_score:?}");
}

#[test]
fn an_absent_summarizer_is_not_an_error_it_is_r_mode() {
    // The rollback guarantee at the summariser level: no model means no semantic
    // fields, and the digest is untouched.
    let cases = dataset();
    let score = run(&cases, &mut NoSummarizer);
    assert_eq!(score.cases, cases.len());
    assert_eq!(
        score.fabricated_abandoned, 0,
        "the computed list does not depend on a model being present"
    );
    assert!(
        score.missing_why > 0,
        "and the absence is scored as a thin `why`, rather than pretended away"
    );
    assert_eq!(
        score.why_matched, 0,
        "with no model, nothing explained the adopted approach"
    );
    assert!(
        !score.faithful(),
        "R mode is honest about being thin: {score:?}"
    );
}

#[test]
fn the_dataset_covers_both_directions() {
    // A gate made of easy cases is not a gate. The dataset must contain cases that
    // punish silence (an abandonment the harness derives, which a faithful `why`
    // must explain) *and* cases that punish invention (nothing was abandoned, and a
    // model that always finds a cause is caught).
    let cases = dataset();
    assert_eq!(
        cases.len(),
        10,
        "ten cases, as the acceptance gate specifies"
    );
    assert!(
        cases.iter().any(|c| !c.abandoned_must_include.is_empty()),
        "some cases have an abandonment the extractor must derive"
    );
    assert!(
        cases.iter().any(|c| c.no_abandoned_expected),
        "some cases have nothing abandoned, so the computed list must be empty"
    );
    assert!(
        cases.iter().any(|c| !c.why_should_mention.is_empty()),
        "some cases have an adopted approach a correct why must name"
    );
    assert!(
        cases.iter().any(|c| !c.why_expected),
        "some cases have no reason to offer, so a model that always finds one is caught"
    );
    assert!(
        cases
            .iter()
            .all(|c| c.no_abandoned_expected == c.abandoned_must_include.is_empty()),
        "an emptiness of expectations and an absence of attempts are the same fact"
    );
    assert!(
        cases
            .iter()
            .all(|c| c.no_abandoned_expected == c.input.edits.is_empty()),
        "a case with nothing abandoned carries no attempt sequence at all"
    );
}

/// The measurement. Ignored by default: it needs a local Ollama with the model
/// pulled, and a gate that depends on a developer's machine is not a gate.
#[test]
#[ignore = "needs a local Ollama; a measurement, not a gate"]
fn faithfulness_of_a_local_model() {
    let base =
        std::env::var("LOONG_OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    // The two candidates measured by hand during the design: the fast one and the
    // quality one. Both are scored so the choice rests on data.
    let models = std::env::var("LOONG_FAITHFULNESS_MODELS")
        .unwrap_or_else(|_| "qwen3:0.6b,qwen2.5:3b".to_string());

    let cases = dataset();
    let mut worst_fabricated = 0usize;
    for model in models.split(',') {
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        let mut summarizer = OllamaSummarizer::new(&base, model).with_max_tokens(300);
        println!("\n=== {model} ===");
        let mut scores = Vec::new();
        for case in &cases {
            match summarizer.summarize(&case.input) {
                Ok(synthesis) => {
                    let score = score(case, &synthesis);
                    let abandoned = computed_abandoned(case);
                    let why = if synthesis.why.is_empty() {
                        "(empty)".to_string()
                    } else {
                        synthesis.why.join("；")
                    };
                    // `n/a` marks a case with no expected phrasing, where a match
                    // cannot be judged either way.
                    let matched = if score.why_total == 0 {
                        "n/a"
                    } else if score.why_matched > 0 {
                        "yes"
                    } else {
                        "no"
                    };
                    println!(
                        "{:20} why_matched={} missing_why={} why={} | abandoned={}",
                        case.name,
                        matched,
                        score.missing_why,
                        why,
                        if abandoned.is_empty() {
                            "(none)".to_string()
                        } else {
                            abandoned.join("；")
                        }
                    );
                    scores.push(score);
                }
                Err(e) => {
                    println!("{:20} ERROR {e}", case.name);
                    scores.push(score(case, &Synthesis::default()));
                }
            }
        }
        let total = sum(&scores);
        println!(
            "-> cases={} fabricated_abandoned={} why_wrong={} missing_why={} why_matched/why_total={}/{} calls={}",
            total.cases,
            total.fabricated_abandoned,
            total.why_wrong,
            total.missing_why,
            total.why_matched,
            total.why_total,
            summarizer.calls()
        );
        if total.fabricated_abandoned > 0 {
            println!(
                "   extractor reported an adopted attempt as abandoned in: {:?}",
                total.fabricated_cases
            );
        }
        if total.why_wrong > 0 {
            println!("   wrong why in: {:?}", total.wrong_cases);
        }
        worst_fabricated = worst_fabricated.max(total.fabricated_abandoned);
    }

    // A floor rather than a target: the gate exists to stop an extractor that
    // misreads a passing attempt from shipping, and this is the assertion that does
    // it. The model's own behaviour is reported above, not asserted, because a
    // measurement is not a gate.
    assert_eq!(
        worst_fabricated, 0,
        "the harness must not report an adopted attempt as abandoned"
    );
}
