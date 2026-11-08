use loong_reverse_formats::sqlite::{CellValue, SqliteDb};
use loong_reverse_testkit::sqlite::{PAGE_SIZE as PAGE, Val, build_record, table_db};

fn database(pages: usize) -> Vec<u8> {
    let mut data = table_db(
        "CREATE TABLE t(value TEXT)",
        &[(1, vec![Val::Text("kept".into())])],
    );
    data.resize(pages * PAGE, 0);
    data
}

fn cell(value: &str) -> Vec<u8> {
    let payload = build_record(&[Val::Text(value.into())]);
    assert!(payload.len() < 128);
    let mut bytes = vec![payload.len() as u8, 1];
    bytes.extend(payload);
    bytes
}

fn put_cell(data: &mut [u8], pointer: usize, bytes: &[u8]) {
    data[PAGE + 8..PAGE + 10].copy_from_slice(&(pointer as u16).to_be_bytes());
    data[PAGE + pointer..PAGE + pointer + bytes.len()].copy_from_slice(bytes);
}

fn assert_unreadable(data: &[u8]) {
    let rows = SqliteDb::parse(data).unwrap().read_rows(2, 0, 10);
    assert!(
        rows.rows.is_empty(),
        "out-of-page bytes must not produce rows: {:?}",
        rows.rows
    );
    assert!(
        !rows.notes.is_empty(),
        "the damaged row/page must be disclosed"
    );
}

#[test]
fn cell_pointer_cannot_reach_the_next_page() {
    let mut data = database(3);
    put_cell(&mut data, PAGE + 100, &cell("borrowed"));
    assert_unreadable(&data);
}

#[test]
fn local_payload_cannot_continue_in_the_next_page() {
    let mut data = database(3);
    put_cell(&mut data, PAGE - 3, &cell("borrowed"));
    assert_unreadable(&data);
}

#[test]
fn reserved_page_bytes_cannot_become_row_data() {
    let mut data = database(2);
    data[20] = 16;
    put_cell(&mut data, PAGE - 16, &cell("secret"));
    assert_unreadable(&data);
}

#[test]
fn cell_pointer_cannot_point_back_into_its_pointer_array() {
    let mut data = database(2);
    data[PAGE + 8..PAGE + 10].copy_from_slice(&8u16.to_be_bytes());
    assert_unreadable(&data);
}

#[test]
fn cell_pointer_array_must_fit_before_the_page_end() {
    let mut data = database(3);
    let cells = (PAGE - 8) / 2 + 1;
    data[PAGE + 3..PAGE + 5].copy_from_slice(&(cells as u16).to_be_bytes());
    let db = SqliteDb::parse(&data).unwrap();
    let count = db.count_rows(2);
    assert!(
        count.bounded,
        "a count from a damaged page must not claim to be complete"
    );
    assert_eq!(count.count, 0);
    assert_unreadable(&data);
}

#[test]
fn interior_child_pointer_cannot_cross_its_page() {
    let mut data = database(4);
    let valid_leaf = data[PAGE..2 * PAGE].to_vec();
    data[3 * PAGE..4 * PAGE].copy_from_slice(&valid_leaf);
    data[PAGE..2 * PAGE].fill(0);
    data[PAGE] = 0x05;
    data[PAGE + 3..PAGE + 5].copy_from_slice(&1u16.to_be_bytes());
    data[PAGE + 12..PAGE + 14].copy_from_slice(&((PAGE - 2) as u16).to_be_bytes());
    data[2 * PAGE - 2..2 * PAGE + 2].copy_from_slice(&4u32.to_be_bytes());
    let rows = SqliteDb::parse(&data).unwrap().read_rows(2, 0, 10);
    assert!(
        rows.rows.is_empty(),
        "a child page number must come entirely from its parent page"
    );
}

#[test]
fn a_cell_may_end_at_the_last_usable_byte() {
    let mut data = database(2);
    data[20] = 16;
    let bytes = cell("kept");
    put_cell(&mut data, PAGE - 16 - bytes.len(), &bytes);
    let rows = SqliteDb::parse(&data).unwrap().read_rows(2, 0, 10);
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(
        rows.rows[0].values[0],
        CellValue::Text {
            text: "kept".into(),
            truncated: false
        }
    );
    assert!(rows.notes.is_empty());
}
