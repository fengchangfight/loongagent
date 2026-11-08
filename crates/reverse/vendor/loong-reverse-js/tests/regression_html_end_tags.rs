use loong_reverse_js::html::scan_html;

#[test]
fn regression_closing_tag_lookalikes_do_not_end_scripts() {
    for lookalike in ["</scripture>", "</script_widget>", "</script:widget>"] {
        let code = format!("const label = '{lookalike}'; window.after = true;");
        let page = format!("<script>{code}</ScRiPt ><script src='next.js'></script>");
        let f = scan_html(&page);
        let (start, end) = f.scripts[0].inline.unwrap();
        assert_eq!(&page[start as usize..end as usize], code);
        assert_eq!(f.scripts.len(), 2);
        assert_eq!(f.scripts[1].src.as_deref(), Some("next.js"));
    }
    assert_eq!(
        scan_html("<title>a</titleish>b</title>").title.as_deref(),
        Some("a</titleish>b")
    );
    let unclosed = scan_html("<title>caption</titleish><script src='ghost.js'></script>");
    assert!(unclosed.scripts.is_empty());
    assert!(
        unclosed
            .title
            .as_deref()
            .unwrap()
            .starts_with("caption</titleish>")
    );
}
