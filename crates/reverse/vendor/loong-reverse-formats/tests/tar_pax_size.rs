use loong_reverse_formats::tarcpio::{member_data, tar};
use loong_reverse_testkit::firmware as fx;

fn clear_member_size(archive: &mut [u8], offset: usize) {
    let header = &mut archive[offset..offset + 512];
    header[124..136].copy_from_slice(b"00000000000\0");
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

#[test]
fn pax_size_survives_a_gnu_long_name_record() {
    let mut archive = fx::tar_entries(&[
        ("PaxHeader", b'x', b"10 size=3\n", ""),
        ("././@LongLink", b'L', b"renamed\0", ""),
        ("short", b'0', b"abc", ""),
        ("next", b'0', b"later", ""),
    ]);
    clear_member_size(&mut archive, 2048);
    let listing = tar(&archive, 10).unwrap();
    assert!(listing.complete, "{:?}", listing.warnings);
    assert_eq!(listing.members.len(), 2);
    assert_eq!(listing.members[0].name, "renamed");
    assert_eq!(listing.members[0].size, 3);
    assert_eq!(
        member_data(&archive, &listing.members[0]),
        Some(b"abc".as_slice())
    );
    assert_eq!(listing.members[1].size, 5);
    assert_eq!(
        member_data(&archive, &listing.members[1]),
        Some(b"later".as_slice())
    );
}

#[test]
fn pax_size_without_intervening_metadata_still_applies_once() {
    let mut archive = fx::tar_entries(&[
        ("PaxHeader", b'x', b"10 size=3\n", ""),
        ("short", b'0', b"abc", ""),
        ("next", b'0', b"later", ""),
    ]);
    clear_member_size(&mut archive, 1024);
    let listing = tar(&archive, 10).unwrap();
    assert!(listing.complete);
    assert_eq!(
        listing.members.iter().map(|m| m.size).collect::<Vec<_>>(),
        vec![3, 5]
    );
}
