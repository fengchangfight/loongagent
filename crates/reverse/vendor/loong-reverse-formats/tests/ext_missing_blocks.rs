use loong_reverse_formats::fsread::{self, Limits};

const FILE_INODE: usize = 5120 + 10 * 128;

#[test]
fn a_nonzero_pointer_outside_the_image_is_missing_data_not_a_sparse_hole() {
    for extents in [false, true] {
        let mut data = loong_reverse_testkit::firmware::ext_image(extents);
        let pointer = FILE_INODE + if extents { 60 } else { 40 };
        data[pointer..pointer + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let image = fsread::extfs::read(&data, &Limits::default());
        let hello = image
            .entries
            .iter()
            .find(|e| e.path == "hello.txt")
            .unwrap();
        assert_eq!(hello.size, 22);
        assert!(hello.data.is_none(), "extent mode {extents}");
        assert!(!image.complete);
        assert!(image.warnings.iter().any(|w| w.contains("block")));
        assert_eq!(
            image
                .entries
                .iter()
                .find(|e| e.path == "sub/note")
                .unwrap()
                .data
                .as_deref(),
            Some(b"nested note".as_slice())
        );
    }
}

#[test]
fn a_true_zero_pointer_still_represents_a_sparse_hole() {
    let mut data = loong_reverse_testkit::firmware::ext_image(false);
    data[FILE_INODE + 40..FILE_INODE + 44].fill(0);
    let image = fsread::extfs::read(&data, &Limits::default());
    let hello = image
        .entries
        .iter()
        .find(|e| e.path == "hello.txt")
        .unwrap();
    assert_eq!(hello.data.as_deref(), Some([0u8; 22].as_slice()));
    assert!(image.complete, "{:?}", image.warnings);
}
