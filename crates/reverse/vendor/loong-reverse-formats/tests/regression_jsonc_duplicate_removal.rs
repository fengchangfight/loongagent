use loong_reverse_formats::jsonc::parse;
use serde_json::json;

#[test]
fn removing_a_setting_cannot_reveal_an_earlier_duplicate() {
    for original in [
        r#"{"server":1,"server":2}"#,
        r#"{"server":1,"keep":3,"server":2}"#,
        r#"{"keep":3,"server":1,"server":2}"#,
        "{\n  \"server\": 1,\n  \"keep\": 3,\n  \"server\": 2\n}\n",
        r#"{"server":1,"\u0073erver":2,"keep":3}"#,
    ] {
        for strict in [true, false] {
            let doc = parse(original, strict).unwrap();
            let edited = doc.remove(&["server"]).unwrap();
            let result = parse(&edited, strict).unwrap();
            assert_eq!(result.get(&["server"]), None, "{original} became {edited}");
            assert_eq!(result.get(&["keep"]), doc.get(&["keep"]));
            assert!(result.remove(&["server"]).is_none());
        }
    }
}

#[test]
fn nested_duplicate_removal_preserves_other_settings_and_comments() {
    let original = "// settings\n{\n  \"servers\": {\n    \"sample\": {\"command\": \"old\"},\n    // keep another server\n    \"other\": {\"command\": \"other-cli\"},\n    \"sample\": {\"command\": \"new\"},\n  },\n  \"theme\": \"dark\",\n}\n";
    let edited = parse(original, false)
        .unwrap()
        .remove(&["servers", "sample"])
        .unwrap();
    let result = parse(&edited, false).unwrap();
    assert_eq!(result.get(&["servers", "sample"]), None);
    assert_eq!(
        result.get(&["servers", "other", "command"]),
        Some(json!("other-cli"))
    );
    assert_eq!(result.get(&["theme"]), Some(json!("dark")));
    assert!(edited.contains("// settings") && edited.contains("// keep another server"));
}

#[test]
fn removing_one_unique_member_keeps_the_existing_behavior() {
    let doc = parse(r#"{"a":1,"b":2}"#, true).unwrap();
    let edited = doc.remove(&["a"]).unwrap();
    assert_eq!(parse(&edited, true).unwrap().to_value(), json!({"b": 2}));
    assert!(doc.remove(&["missing"]).is_none());
}

#[test]
fn adjacent_duplicates_keep_independent_comments() {
    for original in [
        "{\"server\":1, /* keep this explanation */ \"server\":2}",
        "{\"server\":1, // keep this explanation\n \"server\":2,\"keep\":3}",
        "{\"keep\":3,\"server\":1, /* keep this explanation */ \"server\":2}",
        "{\n  \"keep\": 3,\n  \"server\": 1,\n  // keep this explanation\n  \"server\": 2\n}\n",
    ] {
        let doc = parse(original, false).unwrap();
        let edited = doc.remove(&["server"]).unwrap();
        assert!(
            edited.contains("keep this explanation"),
            "{original} became {edited}"
        );
        let result = parse(&edited, false).unwrap();
        assert_eq!(result.get(&["server"]), None);
        assert_eq!(result.get(&["keep"]), doc.get(&["keep"]));
    }

    let original =
        "{\n  \"server\": 1,\n  // keep this explanation\n  \"server\": 2,\n  \"keep\": 3\n}\n";
    let edited = parse(original, false).unwrap().remove(&["server"]).unwrap();
    assert_eq!(edited, "{\n  // keep this explanation\n  \"keep\": 3\n}\n");
}

#[test]
fn comments_around_removed_separators_remain_independent() {
    for original in [
        "{\"server\":1 /* keep value note */, /* keep separator note */ \"server\":2}",
        "{\"keep\":3, /* keep separator note */ \"server\":1,\"server\":2 /* keep value note */}",
    ] {
        let doc = parse(original, false).unwrap();
        let edited = doc.remove(&["server"]).unwrap();
        assert!(
            edited.contains("keep value note") && edited.contains("keep separator note"),
            "{original} became {edited}"
        );
        let result = parse(&edited, false).unwrap();
        assert_eq!(result.get(&["server"]), None);
        assert_eq!(result.get(&["keep"]), doc.get(&["keep"]));
    }
}
