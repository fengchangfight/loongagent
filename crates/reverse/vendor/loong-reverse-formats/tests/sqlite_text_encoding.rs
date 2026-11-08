use loong_reverse_formats::sqlite::{CellValue, SqliteDb};

const PAGE: usize = 8192;

fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = vec![(value & 0x7f) as u8];
    value >>= 7;
    while value != 0 {
        bytes.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    bytes.reverse();
    bytes
}

fn text(value: &str, encoding: u32) -> (u64, Vec<u8>) {
    let bytes: Vec<u8> = match encoding {
        2 => value.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        3 => value.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => value.as_bytes().to_vec(),
    };
    (13 + 2 * bytes.len() as u64, bytes)
}

fn record(fields: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let types: Vec<u8> = fields
        .iter()
        .flat_map(|(serial, _)| varint(*serial))
        .collect();
    assert!(types.len() < 127);
    let mut out = vec![(types.len() + 1) as u8];
    out.extend(types);
    for (_, bytes) in fields {
        out.extend(bytes);
    }
    out
}

fn leaf(page: &mut [u8], header: usize, index: bool, payload: Vec<u8>) {
    let mut cell = varint(payload.len() as u64);
    if !index {
        cell.push(1);
    }
    cell.extend(payload);
    let start = PAGE - cell.len();
    page[header] = if index { 0x0a } else { 0x0d };
    page[header + 3..header + 5].copy_from_slice(&1u16.to_be_bytes());
    page[header + 5..header + 7].copy_from_slice(&(start as u16).to_be_bytes());
    page[header + 8..header + 10].copy_from_slice(&(start as u16).to_be_bytes());
    page[start..].copy_from_slice(&cell);
}

fn database(encoding: u32, value: &str, without_rowid: bool) -> Vec<u8> {
    let mut bytes = vec![0; PAGE * 2];
    bytes[..16].copy_from_slice(b"SQLite format 3\0");
    bytes[16..18].copy_from_slice(&(PAGE as u16).to_be_bytes());
    bytes[18..20].copy_from_slice(&[1, 1]);
    bytes[21..24].copy_from_slice(&[64, 32, 32]);
    bytes[28..32].copy_from_slice(&2u32.to_be_bytes());
    bytes[44..48].copy_from_slice(&4u32.to_be_bytes());
    bytes[56..60].copy_from_slice(&encoding.to_be_bytes());
    let sql = if without_rowid {
        "CREATE TABLE t(value TEXT PRIMARY KEY) WITHOUT ROWID"
    } else {
        "CREATE TABLE t(value TEXT)"
    };
    let schema = record(&[
        text("table", encoding),
        text("t", encoding),
        text("t", encoding),
        (1, vec![2]),
        text(sql, encoding),
    ]);
    leaf(&mut bytes[..PAGE], 100, false, schema);
    leaf(
        &mut bytes[PAGE..],
        0,
        without_rowid,
        record(&[text(value, encoding)]),
    );
    bytes
}

fn check_encoding(encoding: u32) {
    let expected = "水と café 🌱";
    for without_rowid in [false, true] {
        let bytes = database(encoding, expected, without_rowid);
        let db = SqliteDb::parse(&bytes).unwrap();
        let table = db
            .find_object("t")
            .expect("the declared encoding also applies to schema text");
        assert_eq!(table.obj_type, "table");
        assert_eq!(table.columns.len(), 1);
        assert_eq!(table.columns[0].name, "value");
        let rows = db.read_rows(table.rootpage, 0, 10);
        assert!(rows.notes.is_empty(), "{:?}", rows.notes);
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(
            rows.rows[0].values,
            vec![CellValue::Text {
                text: expected.into(),
                truncated: false
            }]
        );
    }
}

#[test]
fn utf16_little_endian_schema_and_rows() {
    check_encoding(2);
}

#[test]
fn utf16_big_endian_schema_and_rows() {
    check_encoding(3);
}

#[test]
fn utf8_schema_and_rows_are_unchanged() {
    check_encoding(1);
}

#[test]
fn utf16_text_keeps_the_source_byte_limit() {
    for encoding in [2, 3] {
        let bytes = database(encoding, &"文".repeat(2050), false);
        let db = SqliteDb::parse(&bytes).unwrap();
        let rows = db.read_rows(2, 0, 1);
        assert_eq!(
            rows.rows[0].values[0],
            CellValue::Text {
                text: "文".repeat(2048),
                truncated: true
            }
        );
    }
}
