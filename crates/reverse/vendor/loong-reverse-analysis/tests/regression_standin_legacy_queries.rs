use loong_reverse_analysis::standin::{self, Answer, MatchKind, StandIn, Turns};
use serde_json::json;

fn saved_v1(first: &str, second: &str) -> StandIn {
    let saved = json!({
        "version": 1,
        "routes": [{
            "method": "GET", "template": "/feed",
            "samples": [
                {"path": "/feed", "query": first, "status": 200, "text": "first-page"},
                {"path": "/feed", "query": second, "status": 200, "text": "second-page"}
            ]
        }]
    });
    serde_json::from_str(&saved.to_string()).unwrap()
}

fn expect_answer(saved: &StandIn, query: &str, expected: &[u8], how: MatchKind) {
    let Answer::Found(found) =
        standin::answer(saved, &mut Turns::default(), "GET", "/feed", query, b"")
    else {
        panic!("the saved request should match");
    };
    assert_eq!(found.body, expected, "{query}");
    assert_eq!(found.how, how, "{query}");
}

fn recorded_v1(first: &str, second: &str) -> StandIn {
    let entries: Vec<_> = [(first, "first-page"), (second, "second-page")]
        .into_iter()
        .map(|(query, text)| {
            json!({
                "request": {"method": "GET", "url": format!("https://example.test/feed?{query}")},
                "response": {"status": 200, "content": {"mimeType": "application/json", "text": text}}
            })
        })
        .collect();
    let har = serde_json::to_vec(&json!({"log": {"version": "1.2", "entries": entries}})).unwrap();
    let recording = standin::build_from_bytes(&har, &standin::BuildOptions::default()).unwrap();
    serde_json::from_slice(&serde_json::to_vec(&recording).unwrap()).unwrap()
}

#[test]
fn legacy_v1_encoded_credential_queries_select_the_matching_variant() {
    for name in ["%74oken", "a%70i_key", "ACCESS%5fTOKEN"] {
        let saved = saved_v1(
            &format!("{name}=recorded-fake&page=1"),
            &format!("{name}=recorded-fake&page=2"),
        );
        // Ask for the second recording first: path-only fallback would return the first body.
        expect_answer(
            &saved,
            &format!("page=2&{name}=replacement-fake"),
            b"second-page",
            MatchKind::PathAndQuery,
        );
        expect_answer(
            &saved,
            &format!("page=1&{name}=replacement-fake"),
            b"first-page",
            MatchKind::PathAndQuery,
        );
    }
}

#[test]
fn already_scrubbed_and_ordinary_saved_queries_still_match() {
    for (first, second, request) in [
        (
            "%74oken=[dropped]&page=1",
            "%74oken=[dropped]&page=2",
            "page=2&%74oken=replacement-fake",
        ),
        (
            "name=public&page=1",
            "name=public&page=2",
            "page=2&name=public",
        ),
    ] {
        expect_answer(
            &saved_v1(first, second),
            request,
            b"second-page",
            MatchKind::PathAndQuery,
        );
    }
}

#[test]
fn an_unrecorded_query_keeps_the_existing_path_fallback() {
    let saved = saved_v1(
        "%74oken=recorded-fake&page=1",
        "%74oken=recorded-fake&page=2",
    );
    expect_answer(
        &saved,
        "page=3&%74oken=replacement-fake",
        b"first-page",
        MatchKind::Path,
    );
}

#[test]
fn legacy_v1_empty_query_pairs_survive_current_credential_scrubbing() {
    for count in [1, 2] {
        // An explicit '=' was saved as an empty slot before encoded credentials were scrubbed.
        let saved_prefix = "&".repeat(count);
        let request_prefix = "=&".repeat(count);
        let saved = saved_v1(
            &format!("{saved_prefix}%74oken=recorded-fake&page=1"),
            &format!("{saved_prefix}%74oken=recorded-fake&page=2"),
        );
        expect_answer(
            &saved,
            &format!("{request_prefix}%74oken=replacement-fake&page=2"),
            b"second-page",
            MatchKind::PathAndQuery,
        );
        expect_answer(
            &saved,
            &format!("{request_prefix}%74oken=replacement-fake&page=1"),
            b"first-page",
            MatchKind::PathAndQuery,
        );
    }
}

#[test]
fn raw_empty_query_segments_are_not_recorded_as_pairs() {
    let saved = recorded_v1("&&a&&b&&page=1&&", "&&a&&b&&page=2&&");
    for query in ["a&b&page=2", "&&a&&b&&page=2&&"] {
        expect_answer(&saved, query, b"second-page", MatchKind::PathAndQuery);
    }
}

#[test]
fn explicit_empty_query_pairs_still_round_trip_in_new_recordings() {
    for count in [1, 2] {
        let prefix = "=&".repeat(count);
        let saved = recorded_v1(
            &format!("{prefix}%74oken=recorded-fake&page=1"),
            &format!("{prefix}%74oken=recorded-fake&page=2"),
        );
        assert_eq!(
            saved.routes[0].samples[1].query,
            format!("{}%74oken=[dropped]&page=2", "&".repeat(count))
        );
        expect_answer(
            &saved,
            &format!("{prefix}%74oken=replacement-fake&page=2"),
            b"second-page",
            MatchKind::PathAndQuery,
        );
    }
}

#[test]
fn ignored_query_separators_do_not_consume_the_pair_limit() {
    let prefix = "&".repeat(201);
    let saved = recorded_v1(
        &format!("{prefix}%74oken=recorded-fake&page=1"),
        &format!("{prefix}%74oken=recorded-fake&page=2"),
    );
    expect_answer(
        &saved,
        &format!("{prefix}%74oken=replacement-fake&page=2"),
        b"second-page",
        MatchKind::PathAndQuery,
    );
}

#[test]
fn replay_keeps_the_recorders_two_hundred_pair_limit() {
    let prefix = (0..199)
        .map(|i| format!("field{i}=public&"))
        .collect::<String>();
    let saved = recorded_v1(
        &format!("{prefix}page=1&ignored=first"),
        &format!("{prefix}page=2&ignored=second"),
    );
    expect_answer(
        &saved,
        &format!("{prefix}page=2&ignored=replacement"),
        b"second-page",
        MatchKind::PathAndQuery,
    );
}
