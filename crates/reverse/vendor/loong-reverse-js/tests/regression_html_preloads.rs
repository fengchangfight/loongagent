use loong_reverse_js::html::scan_html;

#[test]
fn regression_preload_relationships_are_tokens() {
    let f = scan_html(
        r#"<link rel="alternate PRELOAD" as="SCRIPT" href="one.js"><link rel="stylesheet modulepreload" href="two.js"><link rel="notpreload" as="script" href="no.js"><link rel="preload" as="style" href="no.css"><link rel="preloadish" as="script" href="other.js">"#,
    );
    assert_eq!(f.preloads, ["one.js", "two.js"]);
}
