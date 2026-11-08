use loong_reverse_formats::der::{oid, oid_name};

#[test]
fn unfinished_oid_does_not_alias_a_known_algorithm() {
    let mut algorithm = vec![0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 0x0b];
    assert_eq!(oid_name(&oid(&algorithm)), Some("sha256WithRSAEncryption"));
    algorithm.push(0x81);
    assert_eq!(oid(&algorithm), "invalid");
    assert!(oid_name(&oid(&algorithm)).is_none());
}

#[test]
fn malformed_and_oversized_oids_are_not_returned_as_prefixes() {
    assert_eq!(oid(&[]), "invalid");
    assert_eq!(oid(&[0x80, 0x2a]), "invalid");
    assert_eq!(oid(&[0x2a, 0x80, 0]), "invalid");
    assert_eq!(oid(&[0; 65]), "invalid");
    assert_eq!(oid(&[0xff; 11]), "invalid");
}

#[test]
fn complete_oid_arcs_and_the_length_boundary_remain_supported() {
    assert_eq!(oid(&[0x88, 0x37, 3]), "2.999.3");
    let mut boundary = vec![0x2a];
    boundary.resize(64, 0);
    assert!(oid(&boundary).starts_with("1.2.0.0."));
    assert_eq!(oid(&boundary).split('.').count(), 65);
}
