use loong_reverse_formats::{bytes::Reader, dex, java_class};

fn class_string(encoded: &[u8]) -> String {
    let mut bytes = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52, 0, 2, 1];
    bytes.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
    bytes.extend_from_slice(encoded);
    bytes.extend_from_slice(&[0; 14]);
    let class = java_class::parse(&bytes).unwrap();
    match &class.pool[1] {
        java_class::Const::Utf8(s) => s.clone(),
        other => panic!("unexpected constant {other:?}"),
    }
}

#[test]
fn damaged_sequences_keep_following_ascii_in_both_formats() {
    for bytes in [
        &[0xc2, b'A'][..],
        &[0xe2, b'A', b'B'][..],
        &[0xe2, 0x82, b'A'][..],
    ] {
        let expected = if bytes == [0xc2, b'A'] {
            "\u{fffd}A"
        } else if bytes == [0xe2, b'A', b'B'] {
            "\u{fffd}AB"
        } else {
            "\u{fffd}\u{fffd}A"
        };
        assert_eq!(
            dex::mutf8(&Reader::new(bytes), 0, 32),
            (expected.into(), bytes.len())
        );
        assert_eq!(class_string(bytes), expected);
    }
}

#[test]
fn incomplete_and_overlong_sequences_do_not_invent_characters() {
    for bytes in [
        &[0xc2][..],
        &[0xe2, 0x82][..],
        &[0xc0, 0x81][..],
        &[0xe0, 0x80, 0x80][..],
    ] {
        let expected = "\u{fffd}".repeat(bytes.len());
        assert_eq!(
            dex::mutf8(&Reader::new(bytes), 0, 32),
            (expected.clone(), bytes.len())
        );
        assert_eq!(class_string(bytes), expected);
    }
}

#[test]
fn valid_modified_utf8_keeps_nulls_and_surrogate_pairs() {
    let bytes = [
        b'A', 0xc0, 0x80, 0xc3, 0xa9, 0xe4, 0xb8, 0xad, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80,
    ];
    assert_eq!(
        dex::mutf8(&Reader::new(&bytes), 0, 32),
        ("A\0é中😀".into(), bytes.len())
    );
    assert_eq!(class_string(&bytes), "A\0é中😀");
    assert_eq!(
        dex::mutf8(&Reader::new(b"A\0ignored"), 0, 32),
        ("A".into(), 1)
    );
    assert_eq!(dex::mutf8(&Reader::new(b"ABC"), 0, 2), ("AB".into(), 2));
    assert_eq!(
        dex::mutf8(&Reader::new(b"ABC"), usize::MAX, 2),
        (String::new(), 0)
    );
}
