use loong_reverse_formats::fsread::{self, Limits};

const BS: usize = 1024;
const FILE_INODE: usize = 5120 + 10 * 128;

fn put(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn a_missing_single_indirect_root_keeps_the_whole_sparse_span() {
    let mut data = loong_reverse_testkit::firmware::ext_image(false);
    data.resize(14 * BS, 0);
    put(&mut data, 1024 + 4, 14);
    data[FILE_INODE + 40..FILE_INODE + 100].fill(0);
    put(&mut data, FILE_INODE + 4, (269 * BS) as u32);
    put(&mut data, FILE_INODE + 92, 11);
    put(&mut data, 11 * BS, 12);
    put(&mut data, 12 * BS, 13);
    data[13 * BS..13 * BS + 4].copy_from_slice(b"tail");
    let image = fsread::extfs::read(&data, &Limits::default());
    let recovered = image
        .entries
        .iter()
        .find(|e| e.path == "hello.txt")
        .unwrap()
        .data
        .as_ref()
        .unwrap();
    assert!(recovered[..268 * BS].iter().all(|b| *b == 0));
    assert_eq!(&recovered[268 * BS..268 * BS + 4], b"tail");
    assert!(image.complete, "{:?}", image.warnings);
}

#[test]
fn an_existing_single_indirect_block_keeps_its_original_position() {
    let mut data = loong_reverse_testkit::firmware::ext_image(false);
    data.resize(12 * BS, 0);
    put(&mut data, 1024 + 4, 12);
    data[FILE_INODE + 40..FILE_INODE + 100].fill(0);
    put(&mut data, FILE_INODE + 4, (13 * BS) as u32);
    put(&mut data, FILE_INODE + 88, 11);
    put(&mut data, 11 * BS, 8);
    let image = fsread::extfs::read(&data, &Limits::default());
    let recovered = image
        .entries
        .iter()
        .find(|e| e.path == "hello.txt")
        .unwrap()
        .data
        .as_ref()
        .unwrap();
    assert!(recovered[..12 * BS].iter().all(|b| *b == 0));
    assert_eq!(
        &recovered[12 * BS..12 * BS + 22],
        b"Hello ext filesystem!\n"
    );
}
