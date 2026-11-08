use loong_reverse_formats::asar;

#[test]
fn the_declared_header_must_fit_in_the_file() {
    let mut data = asar::build(&[]);
    data[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    data[8..12].copy_from_slice(&(u32::MAX - 4).to_le_bytes());
    assert!(asar::parse(&data, 10).is_err());
}

#[test]
fn nested_pickle_lengths_must_agree() {
    for payload in [0u32, 4, u32::MAX] {
        let mut data = asar::build(&[("file", b"hello")]);
        data[8..12].copy_from_slice(&payload.to_le_bytes());
        assert!(asar::parse(&data, 10).is_err(), "payload {payload}");
    }
}

#[test]
fn valid_header_padding_and_member_offsets_still_read() {
    for name in ["a", "ab", "abc", "abcd"] {
        let data = asar::build(&[(name, b"hello")]);
        let archive = asar::parse(&data, 10).unwrap();
        let entry = archive.get(name).unwrap();
        assert_eq!(
            asar::file_bytes(&data, &archive, entry),
            Some(b"hello".as_slice())
        );
    }
    assert!(
        asar::parse(&asar::build(&[]), 0)
            .unwrap()
            .entries
            .is_empty()
    );
}
