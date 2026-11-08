use loong_reverse_analysis::web_har::{HarOptions, inspect_har};
use serde_json::{Value, json};

fn report(entries: Value, hidden: &[&str]) -> Value {
    let bytes = serde_json::to_vec(&json!({"log": {"entries": entries}})).unwrap();
    let options = HarOptions {
        hidden_values: hidden.iter().map(|s| s.to_string()).collect(),
        limit: 10,
        max_entries: 10,
        ..Default::default()
    };
    inspect_har(&bytes, &options).unwrap()
}

#[test]
fn normalized_ipv6_hosts_do_not_reveal_declared_values() {
    let secret = "[2001:DB8::BEEF]";
    let output = report(
        json!([
            {"request": {"method": "GET", "url": format!("https://{secret}/items")}, "response": {"status": 200}},
            {"request": {"method": "GET", "url": "https://PUBLIC.Example.test/items"}, "response": {"status": 204}}
        ]),
        &[secret],
    );
    assert_eq!(output["entries"][1]["host"], "public.example.test");
    assert_eq!(
        output["entries"][1]["url"],
        "https://public.example.test/items"
    );
    assert_eq!(output["entries"][0]["host"], "[redacted]");
    assert_eq!(output["entries"][0]["url"], "https://[redacted]/items");
    assert!(
        !output
            .to_string()
            .to_ascii_lowercase()
            .contains(&secret.to_ascii_lowercase())
    );
}

#[test]
fn normalized_mime_parameters_do_not_reveal_declared_values() {
    let secret = "Secret=Value";
    let mime = format!("text/plain; x-private={secret}");
    let output = report(
        json!([{
            "request": {"method": "POST", "url": "https://example.test/items", "postData": {"mimeType": mime, "text": "public"}},
            "response": {"status": 200, "content": {"mimeType": mime, "text": "public"}}
        }]),
        &[secret],
    );
    for side in ["request_body", "response_body"] {
        assert_eq!(
            output["entries"][0][side]["kind"],
            "text/plain; x-private=[redacted]"
        );
        assert_eq!(output["entries"][0][side]["bytes"], 6);
    }
    assert!(
        !output
            .to_string()
            .to_ascii_lowercase()
            .contains("secret=value")
    );
}

#[test]
fn redacted_mime_values_still_select_json_form_and_multipart_outlines() {
    let output = report(
        json!([
            {"request": {"method": "POST", "url": "https://example.test/json", "postData": {"mimeType": "application/json", "text": "{\"label\":\"public\"}"}}, "response": {"status": 200}},
            {"request": {"method": "POST", "url": "https://example.test/form", "postData": {"mimeType": "application/x-www-form-urlencoded", "text": "label=public"}}, "response": {"status": 200}},
            {"request": {"method": "POST", "url": "https://example.test/multipart", "postData": {"mimeType": "multipart/form-data", "params": [{"name": "label", "value": "public"}]}}, "response": {"status": 200}}
        ]),
        &[
            "application/json",
            "application/x-www-form-urlencoded",
            "multipart/form-data",
        ],
    );
    for (index, kind) in ["json", "form", "multipart"].into_iter().enumerate() {
        assert_eq!(output["entries"][index]["request_body"]["kind"], kind);
    }
    assert_eq!(
        output["entries"][1]["request_body"]["fields"],
        json!(["label"])
    );
    assert_eq!(
        output["entries"][2]["request_body"]["fields"],
        json!(["label"])
    );
}
