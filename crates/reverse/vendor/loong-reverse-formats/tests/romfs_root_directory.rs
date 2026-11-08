use loong_reverse_formats::fsread::{self, Limits};

fn root_image(root_name: &[u8]) -> Vec<u8> {
    let mut data = vec![0u8; 112];
    data[..8].copy_from_slice(b"-rom1fs-");
    data[8..12].copy_from_slice(&112u32.to_be_bytes());
    data[16..23].copy_from_slice(b"volume\0");
    data[32..36].copy_from_slice(&1u32.to_be_bytes());
    data[36..40].copy_from_slice(&64u32.to_be_bytes());
    data[48..48 + root_name.len()].copy_from_slice(root_name);
    data[64..68].copy_from_slice(&2u32.to_be_bytes());
    data[72..76].copy_from_slice(&5u32.to_be_bytes());
    data[80..86].copy_from_slice(b"hello\0");
    data[96..101].copy_from_slice(b"world");
    data
}

#[test]
fn dot_named_root_is_a_container_for_its_children() {
    let image = fsread::romfs::read(&root_image(b".\0"), &Limits::default());
    assert!(image.complete, "{:?}", image.warnings);
    assert_eq!(image.entries.len(), 1);
    assert_eq!(image.entries[0].path, "hello");
    assert_eq!(image.entries[0].data.as_deref(), Some(b"world".as_slice()));
}

#[test]
fn an_empty_root_name_is_not_a_reason_to_drop_its_children() {
    let image = fsread::romfs::read(&root_image(b"\0"), &Limits::default());
    assert_eq!(image.entries.len(), 1);
    assert_eq!(image.entries[0].path, "hello");
}

#[test]
fn root_children_still_obey_the_entry_limit() {
    let image = fsread::romfs::read(
        &root_image(b".\0"),
        &Limits {
            max_entries: 0,
            ..Limits::default()
        },
    );
    assert!(image.entries.is_empty());
    assert!(!image.complete);
}
