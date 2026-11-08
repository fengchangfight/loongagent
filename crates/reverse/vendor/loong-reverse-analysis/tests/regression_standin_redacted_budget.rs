use loong_reverse_analysis::standin::{BodyLeftOut, BuildOptions, StandIn, build_from_bytes};
use serde_json::json;

fn capture(bodies: &[&str], max_body: usize, max_total: usize) -> StandIn {
    let entries: Vec<_> = bodies
        .iter()
        .enumerate()
        .map(|(i, body)| {
            json!({
                "request": {"method": "GET", "url": format!("https://example.test/data/{i}")},
                "response": {"status": 200, "content": {"mimeType": "text/plain", "text": body}}
            })
        })
        .collect();
    let options = BuildOptions {
        hidden_values: vec!["x".into()],
        max_body_bytes: max_body,
        max_total_body_bytes: max_total,
        ..Default::default()
    };
    build_from_bytes(
        &serde_json::to_vec(&json!({"log": {"entries": entries}})).unwrap(),
        &options,
    )
    .unwrap()
}

#[test]
fn redaction_cannot_expand_a_body_beyond_its_limit() {
    let standin = capture(&["x", "ok"], 8, 8);
    let samples = &standin.routes[0].samples;
    assert_eq!(samples[0].body_left_out, Some(BodyLeftOut::TooLarge));
    assert!(samples[0].text.is_none());
    assert_eq!(
        samples[1].text.as_deref(),
        Some("ok"),
        "a rejected body must not consume the remaining budget"
    );
}

#[test]
fn redaction_cannot_expand_a_capture_beyond_its_total_limit() {
    let standin = capture(&["x", "ok"], 100, 8);
    let samples = &standin.routes[0].samples;
    assert_eq!(samples[0].body_left_out, Some(BodyLeftOut::FileLimit));
    assert!(samples[0].text.is_none());
    assert_eq!(
        samples[1].text.as_deref(),
        Some("ok"),
        "a rejected body must not consume the remaining budget"
    );
}

#[test]
fn the_exact_retained_size_fits_and_leaves_no_space_for_another_body() {
    let standin = capture(&["x", "ok"], 100, 9);
    let samples = &standin.routes[0].samples;
    assert_eq!(samples[0].text.as_deref(), Some("[dropped]"));
    assert_eq!(samples[1].body_left_out, Some(BodyLeftOut::FileLimit));
    assert_eq!(
        samples
            .iter()
            .filter_map(|s| s.text.as_ref())
            .map(String::len)
            .sum::<usize>(),
        9
    );
}

#[test]
fn an_ordinary_body_at_the_limit_is_retained() {
    let standin = capture(&["hello"], 5, 5);
    assert_eq!(standin.routes[0].samples[0].text.as_deref(), Some("hello"));
    assert_eq!(standin.routes[0].samples[0].body_left_out, None);
}
