use loong_reverse_formats::fsread::{self, Limits};

#[test]
fn a_file_skipped_by_its_size_budget_keeps_its_declared_size() {
    let data = loong_reverse_testkit::firmware::jffs2_image();
    for limits in [
        Limits {
            max_file_bytes: 0,
            ..Limits::default()
        },
        Limits {
            max_total_bytes: 0,
            ..Limits::default()
        },
    ] {
        let image = fsread::jffs2::read(&data, &limits);
        let file = image
            .entries
            .iter()
            .find(|e| e.path == "config/settings")
            .unwrap();
        assert_eq!(file.size, 10);
        assert!(file.data.is_none());
        assert!(!image.complete);
    }
}

#[test]
fn the_latest_inode_size_is_preserved_when_bytes_cannot_be_recovered() {
    let mut data = loong_reverse_testkit::firmware::jffs2_image();
    let inode = data
        .windows(4)
        .position(|w| w == [0x85, 0x19, 0x02, 0xe0])
        .unwrap();
    let mut newest = data[inode..].to_vec();
    newest[16..20].copy_from_slice(&2u32.to_le_bytes());
    newest[28..32].copy_from_slice(&20u32.to_le_bytes());
    data.extend_from_slice(&newest);
    let image = fsread::jffs2::read(
        &data,
        &Limits {
            max_file_bytes: 8,
            ..Limits::default()
        },
    );
    let file = image
        .entries
        .iter()
        .find(|e| e.path == "config/settings")
        .unwrap();
    assert_eq!(file.size, 20);
    assert!(file.data.is_none());
}

#[test]
fn recovered_file_sizes_still_match_the_bytes() {
    let image = fsread::jffs2::read(
        &loong_reverse_testkit::firmware::jffs2_image(),
        &Limits::default(),
    );
    let file = image
        .entries
        .iter()
        .find(|e| e.path == "config/settings")
        .unwrap();
    assert_eq!(file.size, 10);
    assert_eq!(file.data.as_deref(), Some(b"key=value\n".as_slice()));
}
