use peep::document::{DocumentFormat, DocumentSource, LoadOptions, open_document};
use peep::search::{SearchDirection, find_literal};
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

    let found =
        find_literal(loaded.document().text(), "第二章", 0, SearchDirection::Forward).unwrap();
    viewport.goto_byte(loaded.document().text(), found.start);
    assert!(viewport.progress_percent(loaded.document().text()) > 0.0);
}
