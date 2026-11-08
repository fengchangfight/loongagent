use loong_reverse_formats::{plist, sourcemap};

#[test]
fn base64_decoders_reject_damaged_suffixes_and_incomplete_quanta() {
    for decode in [plist::base64_decode, sourcemap::base64_decode] {
        for encoded in [
            "A",
            "AAAAA",
            "=",
            "====",
            "YQ=",
            "YQ===",
            "YWJj=",
            "YQ==garbage",
            "YQ==!",
            "YW=Jj",
        ] {
            assert!(decode(encoded).is_none(), "accepted {encoded:?}");
        }
    }
}

#[test]
fn valid_padded_unpadded_and_wrapped_data_is_preserved() {
    for decode in [plist::base64_decode, sourcemap::base64_decode] {
        for (encoded, expected) in [
            ("", ""),
            ("YQ", "a"),
            ("YQ==", "a"),
            ("YWI", "ab"),
            ("YWI=", "ab"),
            ("YWJj", "abc"),
            (" Y W\nI=\r\t", "ab"),
        ] {
            assert_eq!(decode(encoded), Some(expected.as_bytes().to_vec()));
        }
        assert_eq!(decode("+/8="), Some(vec![251, 255]));
    }
    assert_eq!(sourcemap::base64_decode("-_8="), Some(vec![251, 255]));
    assert!(plist::base64_decode("-_8=").is_none());
}

#[test]
fn malformed_embedded_data_is_reported_by_the_public_parsers() {
    assert!(plist::parse_xml(b"<plist><data>YQ==junk</data></plist>").is_err());
    assert!(sourcemap::decode_data_url("data:application/json;base64,e30=!").is_none());
}
