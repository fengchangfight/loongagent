use loong_reverse_analysis::standin::{
    Answer, BodyLeftOut, BuildOptions, NoMatchReason, StandIn, Turns, answer, build_from_bytes,
};
use serde_json::json;

fn capture(responses: &[(u16, &str, Option<&str>)], max_total: usize) -> StandIn {
    let entries: Vec<_> = responses
        .iter()
        .map(|(status, body, encoding)| {
            json!({
                "request": {"method": "GET", "url": "https://example.test/resource"},
                "response": {"status": status, "content": {"mimeType": "text/plain", "text": body, "encoding": encoding}}
            })
        })
        .collect();
    let options = BuildOptions {
        hidden_values: vec!["x".into()],
        max_body_bytes: 100,
        max_total_body_bytes: max_total,
        ..Default::default()
    };
    build_from_bytes(
        &serde_json::to_vec(&json!({"log": {"entries": entries}})).unwrap(),
        &options,
    )
    .unwrap()
}

fn retained_bytes(standin: &StandIn) -> usize {
    standin
        .routes
        .iter()
        .flat_map(|r| &r.samples)
        .map(|s| s.text.as_ref().map_or(0, String::len) + s.base64.as_ref().map_or(0, String::len))
        .sum()
}

#[test]
fn repeated_redacted_bodies_replay_at_the_exact_total_limit() {
    for total in [9, 10] {
        let standin = capture(&[(200, "x", None); 2], total);
        assert_eq!(standin.routes[0].requests, 2);
        assert_eq!(standin.routes[0].samples.len(), 1);
        assert_eq!(retained_bytes(&standin), 9);
        let mut turns = Turns::default();
        for _ in 0..4 {
            let Answer::Found(found) = answer(&standin, &mut turns, "GET", "/resource", "", b"")
            else {
                panic!("missing repeated response");
            };
            assert_eq!(found.status, 200);
            assert_eq!(found.body.as_slice(), b"[dropped]");
            assert_eq!(found.body_left_out, None);
            assert_eq!(found.of, 1);
        }
    }
}

#[test]
fn duplicate_bodies_leave_the_remaining_budget_for_distinct_samples() {
    for (body, retained) in [("x", "[dropped]"), ("hello", "hello")] {
        let total = retained.len() + 2;
        let standin = capture(
            &[(200, body, None), (200, body, None), (200, "ok", None)],
            total,
        );
        let samples = &standin.routes[0].samples;
        assert_eq!(standin.routes[0].requests, 3);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].text.as_deref(), Some(retained));
        assert_eq!(samples[1].text.as_deref(), Some("ok"));
        assert!(samples.iter().all(|sample| sample.body_left_out.is_none()));
        assert_eq!(retained_bytes(&standin), total);
    }
}

#[test]
fn distinct_over_budget_bodies_leave_no_charge_or_duplicate_omission() {
    let standin = capture(
        &[
            (200, "x", None),
            (200, "xx", None),
            (200, "xxx", None),
            (200, "ok", None),
        ],
        11,
    );
    let samples = &standin.routes[0].samples;
    assert_eq!(standin.routes[0].requests, 4);
    assert_eq!(samples.len(), 3);
    assert_eq!(samples[0].text.as_deref(), Some("[dropped]"));
    assert_eq!(samples[1].body_left_out, Some(BodyLeftOut::FileLimit));
    assert!(samples[1].text.is_none());
    assert!(samples[1].base64.is_none());
    assert_eq!(samples[2].text.as_deref(), Some("ok"));
    assert_eq!(retained_bytes(&standin), 11);
}

#[test]
fn budget_rejected_cached_304s_remain_unavailable_and_deduplicated() {
    for total in [0, 8] {
        let standin = capture(&[(304, "x", None); 2], total);
        let route = &standin.routes[0];
        assert_eq!(route.statuses, vec![304]);
        assert_eq!(route.samples.len(), 1);
        assert_eq!(route.samples[0].status, 304);
        assert!(!route.samples[0].was_not_modified);
        assert_eq!(route.samples[0].body_left_out, Some(BodyLeftOut::FileLimit));
        assert!(route.samples[0].text.is_none());
        assert_eq!(retained_bytes(&standin), 0);
        let result = answer(&standin, &mut Turns::default(), "GET", "/resource", "", b"");
        assert!(
            matches!(result, Answer::NoMatch(missing) if missing.reason == NoMatchReason::CachedBodyUnavailable)
        );
    }
}

#[test]
fn repeated_cached_304_bodies_keep_their_usable_replay() {
    let standin = capture(&[(304, "x", None); 2], 9);
    let route = &standin.routes[0];
    assert_eq!(route.statuses, vec![200]);
    assert_eq!(route.samples.len(), 1);
    assert!(route.samples[0].was_not_modified);
    assert_eq!(route.samples[0].text.as_deref(), Some("[dropped]"));
    let mut turns = Turns::default();
    for _ in 0..3 {
        let Answer::Found(found) = answer(&standin, &mut turns, "GET", "/resource", "", b"") else {
            panic!("missing cached response");
        };
        assert_eq!(found.status, 200);
        assert_eq!(found.body.as_slice(), b"[dropped]");
        assert_eq!(found.body_left_out, None);
    }
}

#[test]
fn duplicate_binary_samples_charge_only_the_retained_encoded_bytes() {
    let duplicate = capture(&[(200, "/w==", Some("base64")); 2], 4);
    assert_eq!(duplicate.routes[0].samples.len(), 1);
    assert_eq!(retained_bytes(&duplicate), 4);
    let mut turns = Turns::default();
    for _ in 0..2 {
        let Answer::Found(found) = answer(&duplicate, &mut turns, "GET", "/resource", "", b"")
        else {
            panic!("missing binary response");
        };
        assert_eq!(found.status, 200);
        assert_eq!(found.body.as_slice(), &[0xff]);
        assert_eq!(found.body_left_out, None);
    }
    let standin = capture(
        &[
            (200, "/w==", Some("base64")),
            (200, "/w==", Some("base64")),
            (200, "/g==", Some("base64")),
            (200, "ok", None),
        ],
        6,
    );
    let samples = &standin.routes[0].samples;
    assert_eq!(standin.routes[0].requests, 4);
    assert_eq!(samples.len(), 3);
    assert_eq!(samples[0].base64.as_deref(), Some("/w=="));
    assert_eq!(samples[1].body_left_out, Some(BodyLeftOut::FileLimit));
    assert!(samples[1].text.is_none());
    assert!(samples[1].base64.is_none());
    assert_eq!(samples[2].text.as_deref(), Some("ok"));
    assert_eq!(retained_bytes(&standin), 6);
}
