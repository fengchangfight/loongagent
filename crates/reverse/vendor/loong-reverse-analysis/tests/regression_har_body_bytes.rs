use loong_reverse_analysis::web_har::{HarOptions, inspect_har};
use serde_json::json;

#[test]
fn binary_har_body_size_uses_original_bytes_in_report() {
    let contents = [
        json!({"mimeType": "application/octet-stream", "text": "/wA=", "encoding": "base64"}),
        json!({"mimeType": "text/plain", "text": "é"}),
        json!({"mimeType": "application/json", "text": "eyJvayI6dHJ1ZX0=", "encoding": "base64"}),
    ];
    let entries: Vec<_> = contents
        .into_iter()
        .enumerate()
        .map(|(i, content)| {
            json!({
                "request": {"method": "GET", "url": format!("https://example.test/body/{i}")},
                "response": {"status": 200, "content": content}
            })
        })
        .collect();
    let bytes = serde_json::to_vec(&json!({"log": {"entries": entries}})).unwrap();
    let report = inspect_har(
        &bytes,
        &HarOptions {
            limit: 10,
            max_entries: 10,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report["entries"][0]["response_body"]["bytes"], 2);
    assert_eq!(report["entries"][1]["response_body"]["bytes"], 2);
    assert_eq!(report["entries"][2]["response_body"]["bytes"], 11);
    assert_eq!(
        report["entries"][2]["response_body"]["outline"]["properties"]["ok"],
        "boolean"
    );
}
