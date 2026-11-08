use loong_reverse_js::html::scan_html;

#[test]
fn regression_text_elements_do_not_invent_script_dependencies() {
    for tag in ["style", "textarea", "xmp", "iframe", "noembed", "noframes"] {
        let page = format!(
            "<{tag}><script src='ghost.js'></script></{tag}><script src='real.js'></script>"
        );
        let f = scan_html(&page);
        assert_eq!(f.scripts.len(), 1, "{tag}: {:?}", f.scripts);
        assert_eq!(f.scripts[0].src.as_deref(), Some("real.js"));
    }
    assert!(
        scan_html("<plaintext><script src='ghost.js'></script>")
            .scripts
            .is_empty()
    );
}
