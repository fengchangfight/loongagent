use loong_reverse_analysis::web_har::path_template;

#[test]
fn regression_only_calendar_dates_share_a_date_template() {
    for date in ["2024-02-29", "2000-02-29", "2026-11-03", "2026-04-30"] {
        assert_eq!(
            path_template(&format!("/reports/{date}")),
            "/reports/{date}"
        );
    }
    for literal in [
        "2026-11-news",
        "2026-11-03-extra",
        "2026-02-29",
        "1900-02-29",
        "2026-13-01",
        "2026-00-01",
        "2026-04-31",
        "2026-01-00",
    ] {
        let path = format!("/reports/{literal}");
        assert_eq!(path_template(&path), path);
    }
}
