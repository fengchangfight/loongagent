use loong_reverse_js::html::scan_html;
fn decode_entities(value: &str) -> String {
    scan_html(&format!("<script src=\"{value}\"></script>")).scripts[0]
        .src
        .clone()
        .unwrap()
}

#[test]
fn regression_asset_references_decode_once() {
    let f = scan_html(
        r#"<script src="/app.js?a=1&amp;b=&#x32;&#47;&#51;&amp;quot;"></script><link rel="modulepreload" href="&#x2f;modules&#47;main.js">"#,
    );
    assert_eq!(f.scripts[0].src.as_deref(), Some("/app.js?a=1&b=2/3&quot;"));
    assert_eq!(f.preloads, ["/modules/main.js"]);
    assert_eq!(
        decode_entities("&unknown; &#x; &#not; &amp;amp; &#x1f642;"),
        "&unknown; &#x; &#not; &amp; 🙂"
    );
    assert_eq!(
        decode_entities("&#0; &#xD800; &#x110000; &#x80;"),
        "� � � €"
    );
    assert_eq!(
        decode_entities("&#47app.js &#x2Fmodules.js"),
        "/app.js /modules.js"
    );
    assert_eq!(
        decode_entities(&format!("&#{}47;app.js", "0".repeat(80))),
        "/app.js"
    );
}
