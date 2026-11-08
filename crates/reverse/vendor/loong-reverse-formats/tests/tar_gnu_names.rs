use loong_reverse_formats::tarcpio::{member_data, tar};
use loong_reverse_testkit::firmware as fx;

fn checksum(header: &mut [u8]) {
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

#[test]
fn old_gnu_timestamps_are_not_posix_path_prefixes() {
    let mut archive = fx::tar(&[("program", b"payload")]);
    let header = &mut archive[..512];
    header[257..265].copy_from_slice(b"ustar  \0");
    header[345..357].copy_from_slice(b"00000000123\0");
    header[357..369].copy_from_slice(b"00000000456\0");
    checksum(header);
    let listing = tar(&archive, 10).unwrap();
    assert!(listing.complete);
    assert_eq!(listing.members[0].name, "program");
    assert_eq!(
        member_data(&archive, &listing.members[0]),
        Some(b"payload".as_slice())
    );
}

#[test]
fn posix_ustar_prefixes_are_still_joined() {
    let mut archive = fx::tar(&[("program", b"payload")]);
    let header = &mut archive[..512];
    header[345..352].copy_from_slice(b"usr/bin");
    checksum(header);
    assert_eq!(
        tar(&archive, 10).unwrap().members[0].name,
        "usr/bin/program"
    );
}
