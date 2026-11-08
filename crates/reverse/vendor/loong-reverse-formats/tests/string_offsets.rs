use loong_reverse_formats::strings::{Encoding, extract};

#[test]
fn ascii_offsets_do_not_wrap_past_the_address_space() {
    let strings = extract(b"ok\0later", u64::MAX - 2, 2, 10);
    assert_eq!(strings.len(), 1);
    assert_eq!(strings[0].text, "ok");
    assert_eq!(strings[0].offset, u64::MAX - 2);
    assert!(extract(b"\0later", u64::MAX, 2, 10).is_empty());
}

#[test]
fn wide_offsets_do_not_wrap_past_the_address_space() {
    let strings = extract(b"a\0b\0\0\0c\0d\0", u64::MAX - 4, 2, 10);
    assert_eq!(strings.len(), 1);
    assert_eq!(strings[0].text, "ab");
    assert_eq!(strings[0].encoding, Encoding::Utf16le);
    assert_eq!(strings[0].offset, u64::MAX - 4);
}

#[test]
fn ordinary_offsets_still_identify_both_encodings() {
    let strings = extract(b"one\0t\0w\0o\0", 100, 2, 10);
    assert_eq!(
        strings
            .iter()
            .map(|s| (s.offset, s.text.as_str()))
            .collect::<Vec<_>>(),
        vec![(100, "one"), (104, "two")]
    );
}
