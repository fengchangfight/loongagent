use std::cell::Cell;

use loong_reverse_formats::firmware::{ScanOptions, scan};
use loong_reverse_formats::tarcpio::cpio;
use loong_reverse_testkit::firmware as fx;

const DATA_OFFSET: usize = 112; // 110-byte newc header plus the two-byte name "x\0".

fn crc_archive(payload: &[u8]) -> Vec<u8> {
    let mut archive = fx::cpio_newc(&[("x", payload, 0o100644)]);
    assert_eq!(&archive[DATA_OFFSET..DATA_OFFSET + payload.len()], payload);
    let trailer = (DATA_OFFSET + payload.len() + 3) & !3;
    let sum = payload
        .iter()
        .fold(0u32, |sum, &byte| sum.wrapping_add(u32::from(byte)));
    archive[..6].copy_from_slice(b"070702");
    archive[102..110].copy_from_slice(format!("{sum:08x}").as_bytes());
    archive[trailer..trailer + 6].copy_from_slice(b"070702");
    archive
}

fn odc_archive() -> Vec<u8> {
    let mut archive = Vec::new();
    for (name, payload) in [
        ("plain", b"data".as_slice()),
        ("TRAILER!!!", b"".as_slice()),
    ] {
        let header = format!(
            "070707{dev:06o}{ino:06o}{mode:06o}{uid:06o}{gid:06o}{nlink:06o}{rdev:06o}{mtime:011o}{namesize:06o}{filesize:011o}",
            dev = 0,
            ino = 1,
            mode = 0o100644,
            uid = 0,
            gid = 0,
            nlink = 1,
            rdev = 0,
            mtime = 0,
            namesize = name.len() + 1,
            filesize = payload.len(),
        );
        assert_eq!(header.len(), 76);
        archive.extend_from_slice(header.as_bytes());
        archive.extend_from_slice(name.as_bytes());
        archive.push(0);
        archive.extend_from_slice(payload);
    }
    archive
}

#[test]
fn crc_member_checksum_requires_eight_ascii_hex_digits() {
    for payload in [b"".as_slice(), &[0, 0]] {
        for field in [
            b"+0000000",
            b"0000000+",
            b"0000000g",
            b" 0000000",
            b"0000000\0",
        ] {
            let mut archive = crc_archive(payload);
            archive[102..110].copy_from_slice(field);
            let listing = cpio(&archive, 10).unwrap();
            assert!(listing.complete);
            assert_eq!(listing.members.len(), 1);
            assert!(
                listing
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("unreadable data checksum")),
                "{field:?}: {:?}",
                listing.warnings
            );
            assert!(
                !listing
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("checksum mismatch")),
                "invalid syntax is not a computed mismatch"
            );
        }
    }
}

#[test]
fn crc_checksum_hex_case_and_zero_length_members_remain_valid() {
    for field in [b"000001ba", b"000001BA"] {
        let mut archive = crc_archive(&[255, 171, 16]);
        archive[102..110].copy_from_slice(field);
        let listing = cpio(&archive, 10).unwrap();
        assert!(
            listing.complete && listing.warnings.is_empty(),
            "{listing:?}"
        );
    }
    let empty = crc_archive(&[]);
    let listing = cpio(&empty, 10).unwrap();
    assert!(
        listing.complete && listing.warnings.is_empty(),
        "{listing:?}"
    );
}

#[test]
fn non_crc_variants_do_not_interpret_a_crc_field() {
    let mut newc = fx::cpio_newc(&[("plain", b"data", 0o100644)]);
    newc[102..110].copy_from_slice(b"+0000000");
    for archive in [newc, odc_archive()] {
        let listing = cpio(&archive, 10).unwrap();
        assert!(
            listing.complete && listing.warnings.is_empty(),
            "{listing:?}"
        );
        let result = scan(&archive, &ScanOptions::default(), &|| false);
        let region = result
            .regions
            .iter()
            .find(|region| region.offset == 0)
            .unwrap();
        assert_eq!(region.confidence, "high");
        assert!(result.warnings.is_empty());
    }
}

#[test]
fn overlapping_crc_probes_share_the_scan_byte_budget() {
    // Both checksums are correct. The outer member has no trailer, so its medium-confidence
    // region does not hide the complete inner archive from the scanner.
    let inner = crc_archive(&vec![0x5a; 8192]);
    let mut image = crc_archive(&inner);
    image.truncate((DATA_OFFSET + inner.len() + 3) & !3);
    let plain_offset = image.len();
    image.extend_from_slice(&fx::cpio_newc(&[("plain", b"data", 0o100644)]));
    let empty_offset = image.len();
    image.extend_from_slice(&crc_archive(&[]));

    let complete = cpio(&inner, 10).unwrap();
    assert!(
        complete.complete && complete.warnings.is_empty(),
        "the direct parser still checks the entire inner archive"
    );
    let result = scan(&image, &ScanOptions::default(), &|| false);
    assert!(!result.cancelled && !result.truncated);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("checksum byte budget")),
        "{:?}",
        result.warnings
    );
    let nested = result
        .regions
        .iter()
        .find(|region| region.offset == DATA_OFFSET as u64 && region.kind == "cpio-crc")
        .unwrap();
    assert_eq!(
        nested.details["complete"], true,
        "the trailer is still present"
    );
    assert_eq!(
        nested.confidence, "medium",
        "unverified member data must not gain high confidence"
    );
    assert_eq!(nested.details["checksum_validation_skipped"], true);
    let warnings = nested.details["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("not verified"))
    );
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("checksum mismatch"))
    );
    let visited: u64 = result
        .regions
        .iter()
        .filter(|region| region.kind == "cpio-crc")
        .map(|region| region.details["checksum_bytes_visited"].as_u64().unwrap())
        .sum();
    assert_eq!(
        visited,
        image.len() as u64,
        "the partial probe consumes only the remaining shared allowance"
    );

    let plain = result
        .regions
        .iter()
        .find(|region| region.offset == plain_offset as u64)
        .unwrap();
    assert_eq!(plain.kind, "cpio-newc");
    assert_eq!(plain.confidence, "high");
    let empty = result
        .regions
        .iter()
        .find(|region| region.offset == empty_offset as u64)
        .unwrap();
    assert_eq!(empty.kind, "cpio-crc");
    assert_eq!(empty.confidence, "high");
    assert_eq!(
        empty.details["checksum_validation_skipped"], false,
        "a zero-byte member still verifies with no byte allowance left"
    );
    assert_eq!(empty.details["checksum_bytes_visited"], 0);
    assert!(empty.details["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn crc_body_cancellation_does_not_publish_a_completed_region() {
    let archive = crc_archive(&vec![7; 3 * 64 * 1024]);
    let calls = Cell::new(0usize);
    let cancelled = scan(&archive, &ScanOptions::default(), &|| {
        calls.set(calls.get() + 1);
        calls.get() >= 3
    });
    assert!(
        cancelled.cancelled,
        "the stop callback must be polled inside the checksum body"
    );
    assert_eq!(calls.get(), 3);
    assert!(
        cancelled.regions.is_empty(),
        "a cancelled archive probe must not be reported as complete"
    );

    let finished = scan(&archive, &ScanOptions::default(), &|| false);
    assert!(!finished.cancelled);
    let region = finished
        .regions
        .iter()
        .find(|region| region.offset == 0)
        .unwrap();
    assert_eq!(region.confidence, "high");
    assert_eq!(region.details["complete"], true);
    assert!(region.details["warnings"].as_array().unwrap().is_empty());
}
