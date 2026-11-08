use loong_reverse_formats::axml::StringPool;

fn u32_at(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn pool(utf8: bool, string_data: &[u8], styles: &[u8]) -> Vec<u8> {
    let start = if styles.is_empty() { 32 } else { 36 };
    let mut data = vec![0; start];
    data[..2].copy_from_slice(&1u16.to_le_bytes());
    data[2..4].copy_from_slice(&28u16.to_le_bytes());
    u32_at(&mut data, 8, 1);
    u32_at(&mut data, 12, u32::from(!styles.is_empty()));
    u32_at(&mut data, 16, if utf8 { 1 << 8 } else { 0 });
    u32_at(&mut data, 20, start as u32);
    if !styles.is_empty() {
        u32_at(&mut data, 24, (start + string_data.len()) as u32);
    }
    data.extend(string_data);
    data.extend(styles);
    data.resize(data.len().next_multiple_of(4), 0);
    let size = data.len() as u32;
    u32_at(&mut data, 4, size);
    data
}

fn legacy_utf8(text: &str) -> Vec<u8> {
    let mut data = Vec::new();
    // Legacy AAPT writes the low 15 bits of both lengths even when the full value is larger.
    for len in [text.encode_utf16().count(), text.len()] {
        if len > 0x7f {
            data.push(((len >> 8) & 0x7f) as u8 | 0x80);
        }
        data.push((len & 0xff) as u8);
    }
    data.extend_from_slice(text.as_bytes());
    data.push(0);
    data
}

fn valid_pool() -> Vec<u8> {
    pool(true, b"\x04\x04kept\0", &[])
}

#[test]
fn fixed_fields_must_be_inside_the_declared_header_and_chunk() {
    for header in [8u16, 24] {
        let mut data = valid_pool();
        data[2..4].copy_from_slice(&header.to_le_bytes());
        if header == 8 {
            u32_at(&mut data, 4, 8);
        }
        assert!(StringPool::parse(&data, 0).is_err());
    }
}

#[test]
fn every_declared_offset_must_fit_inside_the_chunk() {
    let mut data = valid_pool();
    u32_at(&mut data, 8, 2);
    u32_at(&mut data, 4, 32);
    assert!(StringPool::parse(&data, 0).is_err());
}

#[test]
fn string_data_cannot_overlap_the_header_or_offset_tables() {
    for start in [0, 16, 28] {
        let mut data = valid_pool();
        u32_at(&mut data, 20, start);
        assert!(StringPool::parse(&data, 0).is_err());
    }
    let mut data = valid_pool();
    u32_at(&mut data, 12, 1);
    assert!(
        StringPool::parse(&data, 0).is_err(),
        "style offsets also take space before string data"
    );
}

#[test]
fn utf8_strings_cannot_borrow_bytes_from_style_data() {
    let data = pool(true, b"\x04\x04ok", b"xy\0\0\0\0\0\0\0\0\0\0");
    let parsed = StringPool::parse(&data, 0).unwrap();
    assert_eq!(parsed.get(0), Some(""));
}

#[test]
fn utf16_strings_cannot_borrow_bytes_from_style_data() {
    let data = pool(false, b"\x02\0o\0", b"k\0\0\0\0\0\0\0\0\0\0\0");
    let parsed = StringPool::parse(&data, 0).unwrap();
    assert_eq!(parsed.get(0), Some(""));
}

#[test]
fn a_pool_at_a_nonzero_offset_uses_only_its_own_bytes() {
    let mut data = vec![0x7f; 13];
    data.extend(valid_pool());
    data.extend([0x5a; 64]);
    assert_eq!(StringPool::parse(&data, 13).unwrap().get(0), Some("kept"));
}

#[test]
fn empty_pools_with_no_string_data_remain_valid() {
    let mut data = vec![0; 28];
    data[..2].copy_from_slice(&1u16.to_le_bytes());
    data[2..4].copy_from_slice(&28u16.to_le_bytes());
    u32_at(&mut data, 4, 28);
    assert!(StringPool::parse(&data, 0).unwrap().strings.is_empty());
}

#[test]
fn legacy_aapt_utf8_recovers_a_65535_byte_string() {
    let expected = "x".repeat(65_535);
    let string_data = legacy_utf8(&expected);
    assert_eq!(&string_data[..4], &[0xff; 4]);
    let parsed = StringPool::parse(&pool(true, &string_data, &[]), 0).unwrap();
    assert_eq!(parsed.get(0), Some(expected.as_str()));
}

#[test]
fn legacy_aapt_utf8_output_remains_capped() {
    let text = "x".repeat(64 * 1024 * 4 + 0x7fff);
    let parsed = StringPool::parse(&pool(true, &legacy_utf8(&text), &[]), 0).unwrap();
    let kept = parsed.get(0).unwrap();
    assert_eq!(kept.len(), 64 * 1024 * 4);
    assert!(kept.bytes().all(|b| b == b'x'));
}

#[test]
fn legacy_aapt_utf8_cannot_borrow_a_terminator_after_its_chunk() {
    let string_data = legacy_utf8(&"x".repeat(65_535));
    let mut data = pool(true, &string_data, &[]);
    let declared_size = data.len() - 4;
    u32_at(&mut data, 4, declared_size as u32);
    assert_eq!(data[data.len() - 1], 0);
    assert_eq!(StringPool::parse(&data, 0).unwrap().get(0), Some(""));
}

#[test]
fn legacy_aapt_utf8_cannot_borrow_a_terminator_from_styles() {
    let mut string_data = legacy_utf8(&"x".repeat(65_532));
    assert_eq!(string_data.pop(), Some(0));
    let data = pool(true, &string_data, &[0; 12]);
    assert_eq!(StringPool::parse(&data, 0).unwrap().get(0), Some(""));
}

#[test]
fn legacy_aapt_utf8_needs_a_terminator_at_a_matching_length() {
    let mut string_data = legacy_utf8(&"x".repeat(65_535));
    *string_data.last_mut().unwrap() = b'x';
    string_data[4 + 40_000] = 0;
    let data = pool(true, &string_data, &[]);
    assert_eq!(StringPool::parse(&data, 0).unwrap().get(0), Some(""));
}

#[test]
fn truncated_utf8_length_prefixes_and_bodies_are_not_recovered() {
    let string_data = legacy_utf8(&"x".repeat(65_535));
    for cut in [0, 1, 2, 3, 4, 32_770, 65_539] {
        let data = pool(true, &string_data[..cut], &[0; 12]);
        assert_eq!(
            StringPool::parse(&data, 0).unwrap().get(0),
            Some(""),
            "cut at {cut}"
        );
    }
}

#[test]
fn ordinary_utf8_lengths_keep_their_exact_content() {
    for text in [
        String::new(),
        "kept".into(),
        "a".repeat(127),
        "b".repeat(128),
        "c".repeat(32_767),
        "🙂".repeat(128),
    ] {
        let parsed = StringPool::parse(&pool(true, &legacy_utf8(&text), &[]), 0).unwrap();
        assert_eq!(parsed.get(0), Some(text.as_str()));
    }
}
