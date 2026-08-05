use peep::document::{
    DocumentFormat, DocumentSource, LoadError, LoadOptions, TextStyleKind, open_document,
};
use peep::search::{SearchDirection, SearchKind, SearchQuery, find_next};
use peep::viewport::Viewport;

#[test]
fn txt_flows_through_the_public_document_interface() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke.txt");
    let loaded = open_document(DocumentSource::from_path(&path), LoadOptions::default()).unwrap();

    assert_eq!(loaded.format(), DocumentFormat::Txt);
    assert!(loaded.document().text().contains("第一章"));
    assert!(!loaded.fingerprint().is_empty());

    let mut viewport = Viewport::new(loaded.document().text(), 0);
    viewport.set_width(20);
    let first_page = viewport.visible_lines(loaded.document().text(), 4);
    assert_eq!(&loaded.document().text()[first_page[0].range()], "第一章");

    let found = find_next(
        loaded.document().text(),
        &SearchQuery::new(SearchKind::LooseLiteral, "第二章"),
        0,
        SearchDirection::Forward,
    )
    .unwrap()
    .unwrap();
    viewport.goto_byte(loaded.document().text(), found.start);
    assert!(viewport.progress_percent(loaded.document().text()) > 0.0);
}

#[test]
fn markdown_flows_through_the_public_document_interface() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke.md");
    let loaded = open_document(DocumentSource::from_path(&path), LoadOptions::default()).unwrap();
    let document = loaded.document();

    assert_eq!(loaded.format(), DocumentFormat::Markdown);
    assert_eq!(document.metadata().title(), Some("Peep Markdown 冒烟文档"));
    assert_eq!(document.metadata().author(), Some("Peep tests"));
    assert_eq!(document.toc().len(), 2);
    assert_eq!(document.toc()[0].label(), "开始阅读");
    assert_eq!(document.toc()[1].label(), "表格");
    assert!(document.text().contains("源码软换行重新排版"));
    assert!(document.text().contains("文档 <https://example.com/docs>"));
    assert!(document.text().contains('┌'));
    assert!(document.text().contains("[Image: 架构图] <assets/architecture.png>"));

    for entry in document.toc() {
        assert!(document.text()[entry.offset()..].starts_with(entry.label()));
    }
    for style in document.styles() {
        let range = style.range();
        assert!(range.start < range.end);
        assert!(range.end <= document.text().len());
        assert!(document.text().is_char_boundary(range.start));
        assert!(document.text().is_char_boundary(range.end));
    }
    assert!(document.styles().iter().any(|style| style.kind() == TextStyleKind::Code));
    assert!(document.styles().iter().any(|style| style.kind() == TextStyleKind::Link));
    assert!(document.styles().iter().any(|style| style.kind() == TextStyleKind::Quote));
    assert!(document.styles().iter().any(|style| style.kind() == TextStyleKind::Strikethrough));
}

#[test]
fn markdown_detection_is_extension_driven_and_requires_utf8() {
    let directory = tempfile::tempdir().unwrap();
    let txt = directory.path().join("markdown-looking.txt");
    std::fs::write(&txt, "# 仍然是 TXT\n").unwrap();
    let loaded = open_document(DocumentSource::from_path(&txt), LoadOptions::default()).unwrap();
    assert_eq!(loaded.format(), DocumentFormat::Txt);
    assert_eq!(loaded.document().text(), "# 仍然是 TXT\n");

    let invalid = directory.path().join("invalid.md");
    std::fs::write(&invalid, [0xff, 0xfe]).unwrap();
    let error = open_document(DocumentSource::from_path(&invalid), LoadOptions::default())
        .expect_err("invalid Markdown must not fall back to the TXT adapter");
    assert!(matches!(error, LoadError::InvalidMarkdownEncoding));
}
