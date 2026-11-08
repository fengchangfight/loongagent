use loong_reverse_js::{Limits, ParseMode, analyze};
use std::fmt::Write;

#[test]
fn ordinary_calls_do_not_exhaust_esbuild_dependency_budget() {
    for (params, format) in [("exports, module", "esbuild-cjs"), ("", "esbuild-esm")] {
        let mut source = format!("var a = h({{\"src/a.js\"({params}) {{\n");
        for i in 0..4096 {
            writeln!(source, "function local{i}() {{}} local{i}();").unwrap();
        }
        // The dependency is declared later, so collecting only already-seen wrappers is insufficient.
        source.push_str(
            "b(); b(); }});\nvar b = h({\"src/b.js\"() { globalThis.ready = true; }});\n",
        );

        let analysis = analyze("bundle.js", &source, &Limits::default());
        assert_eq!(analysis.facts.mode, ParseMode::Parsed);
        assert_eq!(analysis.facts.parse_errors, 0);
        assert_eq!(analysis.bundle.modules.len(), 2);
        let module = analysis
            .bundle
            .modules
            .iter()
            .find(|module| module.id == "src/a.js")
            .unwrap();
        assert_eq!(module.format, format);
        assert_eq!(module.deps, ["src/b.js"]);
    }
}

#[test]
fn esbuild_dependency_limit_keeps_first_distinct_edges() {
    let mut source = String::from("var main = h({\"src/main.js\"() {\n");
    for i in (0..4097).rev() {
        writeln!(source, "m{i}(); m{i}();").unwrap();
    }
    source.push_str("}});\n");
    for i in 0..4097 {
        writeln!(source, "var m{i} = h({{\"src/{i}.js\"() {{}}}});").unwrap();
    }

    let analysis = analyze("bundle.js", &source, &Limits::default());
    assert_eq!(analysis.facts.mode, ParseMode::Parsed);
    assert_eq!(analysis.facts.parse_errors, 0);
    assert_eq!(analysis.bundle.modules.len(), 4098);
    assert!(!analysis.bundle.truncated);
    let module = analysis
        .bundle
        .modules
        .iter()
        .find(|module| module.id == "src/main.js")
        .unwrap();
    let expected: Vec<_> = (1..4097).rev().map(|i| format!("src/{i}.js")).collect();
    assert_eq!(module.deps, expected);
}

#[test]
fn arrow_module_bodies_keep_forward_wrapper_dependencies() {
    let source = r#"
var a = h({"src/a.js": (exports, module) => { b(); b(); }});
var b = h({"src/b.js": () => { globalThis.ready = true; }});
"#;
    let analysis = analyze("bundle.js", source, &Limits::default());
    assert_eq!(analysis.facts.mode, ParseMode::Parsed);
    assert_eq!(analysis.facts.parse_errors, 0);
    assert_eq!(analysis.bundle.modules.len(), 2);
    let module = analysis
        .bundle
        .modules
        .iter()
        .find(|module| module.id == "src/a.js")
        .unwrap();
    assert_eq!(module.format, "esbuild-cjs");
    assert_eq!(module.deps, ["src/b.js"]);
    assert_eq!(
        analysis
            .bundle
            .modules
            .iter()
            .find(|module| module.id == "src/b.js")
            .unwrap()
            .format,
        "esbuild-esm"
    );
}
