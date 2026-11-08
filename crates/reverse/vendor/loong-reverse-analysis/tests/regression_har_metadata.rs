use loong_reverse_analysis::web_har::{HarOptions, inspect_har};
use serde_json::json;

#[test]
fn regression_declared_values_do_not_leak_through_metadata() {
    let secret = "privatevalue";
    let bytes = serde_json::to_vec(&json!({"log": {
        "version": secret, "creator": {"name": secret, "version": secret},
        "entries": [{"startedDateTime": secret, "_resourceType": secret,
            "request": {"method": secret, "url": format!("https://{secret}.test/{secret}?{secret}=safe&%70rivatevalue=safe&%70r%69vatevalue=safe&keep=safe"),
                "httpVersion": secret, "headers": [{"name": secret, "value": "ordinary"}],
                "postData": {"mimeType": format!("text/{secret}"), "text": "ordinary"}},
            "response": {"status": 200, "statusText": secret, "content": {"mimeType": format!("text/{secret}"), "text": "ordinary"}},
            "timings": {(secret): 1.0}}]
    }}))
    .unwrap();
    let report = inspect_har(
        &bytes,
        &HarOptions {
            hidden_values: vec![secret.into()],
            limit: 10,
            max_entries: 10,
            ..Default::default()
        },
    )
    .unwrap();
    let text = report.to_string().to_ascii_lowercase();
    assert!(!text.contains(secret), "{text}");
    assert!(
        !text.contains("%70rivatevalue") && !text.contains("%70r%69vatevalue"),
        "{text}"
    );
    assert_eq!(report["entries"][0]["query_parameters"], json!(["keep"]));
    assert_eq!(report["summary"]["entries"], 1);
    assert_eq!(report["entries"][0]["status"], 200);
    assert!(
        report["entries"][0]["request_headers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
