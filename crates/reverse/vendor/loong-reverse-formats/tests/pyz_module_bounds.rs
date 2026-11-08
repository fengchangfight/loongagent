use loong_reverse_formats::packed::{Compression, PackedFormat};
use loong_reverse_formats::pyinstaller::PyInstaller;
use loong_reverse_testkit::packed::{code_311, marshal as m, pyinstaller};

fn sample() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let code = code_311("<module>", "<module>", "good.py", 1, &[], &[], &[]);
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&code, 6);
    let mut pyz = b"PYZ\0".to_vec();
    pyz.extend_from_slice(&3531u16.to_le_bytes());
    pyz.extend_from_slice(&[0x0d, 0x0a]);
    pyz.extend_from_slice(&[0; 8]);
    pyz.extend_from_slice(&compressed);
    let table_at = pyz.len();
    pyz[8..12].copy_from_slice(&(table_at as u32).to_be_bytes());
    let table = |outside: i32| {
        m::list(vec![
            m::tuple(vec![
                m::string("good"),
                m::tuple(vec![m::int(0), m::int(16), m::int(compressed.len() as i32)]),
            ]),
            m::tuple(vec![
                m::string("outside"),
                m::tuple(vec![
                    m::int(0),
                    m::int(outside),
                    m::int(compressed.len() as i32),
                ]),
            ]),
        ])
    };
    let full_len = table_at + table(0).len();
    pyz.extend_from_slice(&table(full_len as i32));
    let container = pyinstaller(
        &[
            ("PYZ.pyz", b'z', &pyz, false),
            ("neighbor", b'x', &compressed, false),
        ],
        true,
        312,
    );
    (container, compressed, code)
}

#[test]
fn a_module_cannot_read_compressed_bytes_from_the_next_container_member() {
    let (data, _, _) = sample();
    let listing = PyInstaller.list(&data).unwrap();
    let bad = listing
        .entries
        .iter()
        .find(|e| e.path == "PYZ.pyz.modules/outside.pyc")
        .unwrap();
    assert_eq!(bad.compression, Compression::Unreadable);
    assert!(PyInstaller.read(&data, bad, 1000).is_err());
    assert!(
        listing
            .notes
            .iter()
            .any(|n| n.contains("outside") || n.contains("bounds"))
    );
}

#[test]
fn sound_modules_and_neighboring_members_still_read() {
    let (data, compressed, code) = sample();
    let listing = PyInstaller.list(&data).unwrap();
    let good = listing
        .entries
        .iter()
        .find(|e| e.path == "PYZ.pyz.modules/good.pyc")
        .unwrap();
    let recovered = PyInstaller.read(&data, good, 1000).unwrap();
    assert_eq!(&recovered[16..], code);
    let neighbor = listing
        .entries
        .iter()
        .find(|e| e.path == "neighbor")
        .unwrap();
    assert_eq!(PyInstaller.read(&data, neighbor, 1000).unwrap(), compressed);
}
