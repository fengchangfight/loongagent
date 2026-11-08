use loong_reverse_formats::sourcemap::{Limits, decode_data_url, parse};

#[test]
fn percent_encoded_inline_maps_recover_original_sources() {
    let json = r#"{"version":3,"sources":["café.js"],"sourcesContent":["a+b"],"mappings":"AAAA"}"#;
    let payload: String = json.bytes().map(|b| format!("%{b:02X}")).collect();
    let decoded =
        decode_data_url(&format!("data:application/json;charset=utf-8,{payload}")).unwrap();
    let map = parse(&decoded, Limits::default()).unwrap();
    assert_eq!(map.source_path(0).as_deref(), Some("café.js"));
    assert_eq!(map.source_content(0), Some("a+b"));
    assert_eq!(map.original_positions(0, 0).len(), 1);
}

#[test]
fn percent_escapes_are_decoded_before_base64_and_only_once() {
    assert_eq!(
        decode_data_url("data:application/json;base64,%65%33%30%3d"),
        Some(b"{}".to_vec())
    );
    assert_eq!(
        decode_data_url("data:,%25%32%30+a%2Bb"),
        Some(b"%20+a+b".to_vec())
    );
    assert_eq!(decode_data_url("data:,%00%ff"), Some(vec![0, 255]));
    assert_eq!(
        decode_data_url("data:application/json,{}"),
        Some(b"{}".to_vec())
    );
}

#[test]
fn incomplete_percent_escapes_are_refused() {
    for url in [
        "data:,%",
        "data:,%2",
        "data:,%xz",
        "data:application/json;base64,%ff",
    ] {
        assert!(decode_data_url(url).is_none(), "accepted {url}");
    }
}
