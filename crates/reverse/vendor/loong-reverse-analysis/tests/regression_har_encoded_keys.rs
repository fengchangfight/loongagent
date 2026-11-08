use loong_reverse_analysis::web_har::{HarOptions, inspect_har};
use serde_json::json;

#[test]
fn encoded_credential_names_are_redacted_in_the_report() {
    let bytes = serde_json::to_vec(&json!({"log": {"entries": [{
        "request": {"method": "GET", "url": "https://example.test/?%74oken=TOP_SECRET&a%70i_key=KEY_SECRET&ACCESS%5fTOKEN=OTHER_SECRET&name=public&broken%7=ok&description=safe"},
        "response": {"status": 200}
    }]}})).unwrap();
    let report = inspect_har(
        &bytes,
        &HarOptions {
            limit: 10,
            max_entries: 10,
            ..Default::default()
        },
    )
    .unwrap();
    let url = report["entries"][0]["url"].as_str().unwrap();
    assert!(!report.to_string().contains("SECRET"));
    assert_eq!(url.matches("[redacted]").count(), 3);
    for public in ["name=public", "broken%7=ok", "description=safe"] {
        assert!(url.contains(public), "{url}");
    }
}
