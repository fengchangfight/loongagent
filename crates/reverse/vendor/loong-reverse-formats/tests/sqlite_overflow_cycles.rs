use loong_reverse_formats::sqlite::{CellValue, SqliteDb};
use loong_reverse_testkit::sqlite::{PAGE_SIZE as PAGE, Val, overflow_row_db};

fn point(data: &mut [u8], from: usize, to: u32) {
    let start = (from - 1) * PAGE;
    data[start..start + 4].copy_from_slice(&to.to_be_bytes());
}

#[test]
fn a_self_referential_overflow_page_is_read_only_once() {
    let mut data = overflow_row_db(&[Val::Blob(vec![0x39; 9000])]);
    point(&mut data, 3, 3);
    let rows = SqliteDb::parse(&data).unwrap().read_rows(2, 0, 1);
    assert_eq!(
        rows.rows[0].values,
        vec![CellValue::PartialBlob { len: 9000 }]
    );
    assert!(
        rows.notes.iter().any(|note| note.contains("repeats")),
        "{:?}",
        rows.notes
    );
}

#[test]
fn a_two_page_overflow_cycle_is_not_used_to_complete_a_value() {
    let mut data = overflow_row_db(&[Val::Blob(vec![0x47; 17_000])]);
    point(&mut data, 4, 3);
    let rows = SqliteDb::parse(&data).unwrap().read_rows(2, 0, 1);
    assert_eq!(
        rows.rows[0].values,
        vec![CellValue::PartialBlob { len: 17_000 }]
    );
    assert!(!rows.notes.is_empty());
}

#[test]
fn a_cycle_preserves_complete_prefix_values_and_marks_missing_suffixes() {
    let mut data = overflow_row_db(&[
        Val::Text("kept".into()),
        Val::Blob(vec![0x39; 17_000]),
        Val::Int(73),
    ]);
    point(&mut data, 4, 3);
    let rows = SqliteDb::parse(&data).unwrap().read_rows(2, 0, 1);
    assert_eq!(
        rows.rows[0].values,
        vec![
            CellValue::Text {
                text: "kept".into(),
                truncated: false
            },
            CellValue::PartialBlob { len: 17_000 },
            CellValue::NotRead
        ]
    );
}

#[test]
fn distinct_overflow_pages_still_recover_the_whole_blob() {
    let data = overflow_row_db(&[Val::Blob(vec![0x59; 9000])]);
    let rows = SqliteDb::parse(&data).unwrap().read_rows(2, 0, 1);
    assert!(matches!(
        rows.rows[0].values[0],
        CellValue::Blob { len: 9000, .. }
    ));
    assert!(rows.notes.is_empty());
}
