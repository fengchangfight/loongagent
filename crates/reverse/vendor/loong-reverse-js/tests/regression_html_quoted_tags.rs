use loong_reverse_js::html::scan_html;

#[test]
fn regression_markup_in_attributes_is_not_scanned_as_tags() {
    for extra in [
        String::new(),
        (0..70).map(|i| format!(" a{i}='value'")).collect(),
    ] {
        let page = format!(
            "<div{extra} data-example=\"<script src='ghost.js'></script>\"><script src='real.js'></script></div>"
        );
        let f = scan_html(&page);
        assert_eq!(f.scripts.len(), 1, "{:?}", f.scripts);
        assert_eq!(f.scripts[0].src.as_deref(), Some("real.js"));
    }
    for text in ["1 < 2", "1 <2", "x < y"] {
        let page = format!("<p>{text} <script src='real.js'></script></p>");
        assert_eq!(scan_html(&page).scripts[0].src.as_deref(), Some("real.js"));
    }
}
