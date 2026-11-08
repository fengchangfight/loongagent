use loong_reverse_formats::jsonc::parse;
use serde_json::json;

#[test]
fn the_first_setting_keeps_comment_only_documents() {
    for original in [
        "// local preferences\n",
        "// local preferences",
        "/* keep this explanation */\n\n",
        "\u{feff}// local preferences\n",
        "\u{feff}",
        " \t\r\n",
    ] {
        let edited = parse(original, false)
            .unwrap()
            .set(
                &["mcp", "servers", "sample"],
                &json!({"command": "sample-cli"}),
            )
            .unwrap();
        assert!(
            edited.starts_with(original),
            "lost the original prefix: {edited:?}"
        );
        assert_eq!(
            parse(&edited, false)
                .unwrap()
                .get(&["mcp", "servers", "sample"]),
            Some(json!({"command": "sample-cli"}))
        );
    }
}

#[test]
fn a_truly_empty_file_still_gets_a_fresh_object() {
    let edited = parse("", true)
        .unwrap()
        .set(&["enabled"], &json!(true))
        .unwrap();
    assert_eq!(edited, "{\n  \"enabled\": true\n}\n");
    assert_eq!(
        parse(&edited, true).unwrap().get(&["enabled"]),
        Some(json!(true))
    );
}
