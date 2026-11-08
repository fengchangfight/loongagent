use loong_reverse_formats::firmware::{ScanOptions, scan};
use loong_reverse_formats::tarcpio::{cpio, member_data};
use loong_reverse_testkit::firmware as fx;

fn crc_archive() -> Vec<u8> {
    let mut archive = fx::cpio_newc(&[
        ("file", &[255, 128, 65], 0o100644),
        ("link", b"file", 0o120777),
    ]);
    let mut offset = 0;
    for expected in [448u32, 416, 0] {
        let size = usize::from_str_radix(
            std::str::from_utf8(&archive[offset + 54..offset + 62]).unwrap(),
            16,
        )
        .unwrap();
        let name = usize::from_str_radix(
            std::str::from_utf8(&archive[offset + 94..offset + 102]).unwrap(),
            16,
        )
        .unwrap();
        archive[offset..offset + 6].copy_from_slice(b"070702");
        archive[offset + 102..offset + 110].copy_from_slice(format!("{expected:08x}").as_bytes());
        let data = (offset + 110 + name + 3) & !3;
        offset = (data + size + 3) & !3;
    }
    archive
}

#[test]
fn crc_members_report_changed_data_without_losing_the_inventory() {
    let mut archive = crc_archive();
    let clean = cpio(&archive, 10).unwrap();
    let first = clean.members[0].data_offset as usize;
    archive[first] = 0;
    let listing = cpio(&archive, 10).unwrap();
    assert!(listing.complete);
    assert_eq!(listing.members.len(), 2);
    assert!(
        listing
            .warnings
            .iter()
            .any(|w| w.contains("file") && w.contains("checksum")),
        "{:?}",
        listing.warnings
    );
    assert_eq!(
        member_data(&archive, &listing.members[0]),
        Some([0, 128, 65].as_slice())
    );
}

#[test]
fn crc_checks_cover_symlink_data_and_valid_unsigned_bytes() {
    let mut archive = crc_archive();
    let clean = cpio(&archive, 10).unwrap();
    assert!(clean.complete);
    assert!(clean.warnings.is_empty(), "{:?}", clean.warnings);
    let link = clean.members[1].data_offset as usize;
    archive[link] = b'F';
    assert!(
        cpio(&archive, 10)
            .unwrap()
            .warnings
            .iter()
            .any(|w| w.contains("link") && w.contains("checksum"))
    );
}

#[test]
fn newc_has_no_data_checksum_and_truncated_data_is_not_validated() {
    let mut newc = fx::cpio_newc(&[("file", b"abc", 0o100644)]);
    newc[102..110].copy_from_slice(b"00000126");
    assert!(cpio(&newc, 10).unwrap().warnings.is_empty());
    let archive = crc_archive();
    let data = cpio(&archive, 10).unwrap().members[0].data_offset as usize;
    let partial = cpio(&archive[..data + 2], 10).unwrap();
    assert!(!partial.complete);
    assert!(
        !partial
            .warnings
            .iter()
            .any(|w| w.contains("checksum mismatch"))
    );
}

#[test]
fn firmware_inventory_exposes_checksum_damage() {
    let mut archive = crc_archive();
    let clean = scan(&archive, &ScanOptions::default(), &|| false);
    assert_eq!(
        clean
            .regions
            .iter()
            .find(|r| r.offset == 0 && r.kind == "cpio-crc")
            .unwrap()
            .confidence,
        "high"
    );
    let data = cpio(&archive, 10).unwrap().members[0].data_offset as usize;
    archive[data] = 0;
    let damaged = scan(&archive, &ScanOptions::default(), &|| false);
    let region = damaged
        .regions
        .iter()
        .find(|r| r.offset == 0 && r.kind == "cpio-crc")
        .unwrap();
    assert_eq!(region.confidence, "high");
    assert!(
        region.details["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("checksum mismatch"))
    );
}
