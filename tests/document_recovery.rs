use std::io::{Cursor, Write};

use peep::document::{DocumentSource, LoadOptions, open_document};
use peep::viewport::Viewport;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

#[test]
fn xhtml_self_closing_script_does_not_hide_the_book_body() {
    let loaded = open_test_epub(&[
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/book.opf",
            package_xml(
                r#"<item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>"#,
                r#"<itemref idref="chapter"/>"#,
            ),
        ),
        (
            "OPS/chapter.xhtml",
            r#"<?xml version="1.0" encoding="utf-8"?>
               <html xmlns="http://www.w3.org/1999/xhtml">
                 <head><script src="reader.js"/></head>
                 <body><h1>第一章 雨夜</h1><p>故事从这里开始。</p></body>
               </html>"#
                .to_owned(),
        ),
    ]);

    assert!(loaded.document().text().contains("故事从这里开始。"));
    assert_jump_shows(&loaded, "第一章 雨夜", "第一章 雨夜");
}

#[test]
fn dtd_bearing_ncx_keeps_its_navigation_depth_and_targets() {
    let loaded = open_test_epub(&[
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/book.opf",
            r#"<?xml version="1.0"?>
               <package>
                 <metadata><title>DTD fixture</title></metadata>
                 <manifest>
                   <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
                   <item id="one" href="chapter.xhtml" media-type="application/xhtml+xml"/>
                 </manifest>
                 <spine toc="ncx"><itemref idref="one"/></spine>
               </package>"#
                .to_owned(),
        ),
        (
            "OPS/toc.ncx",
            r#"<?xml version="1.0"?>
               <!DOCTYPE ncx PUBLIC "-//NISO//DTD ncx 2005-1//EN" "http://www.daisy.org/z3986/2005/ncx-2005-1.dtd">
               <ncx><navMap>
                 <navPoint><navLabel><text>第一章 风起</text></navLabel><content src="chapter.xhtml#one"/></navPoint>
                 <navPoint><navLabel><text>第二章 云涌</text></navLabel><content src="chapter.xhtml#two"/></navPoint>
               </navMap></ncx>"#
                .to_owned(),
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
                 <h2 id="one">第一章 风起</h2><p>一。</p>
                 <h2 id="two">第二章 云涌</h2><p>二。</p>
               </body></html>"#
                .to_owned(),
        ),
    ]);

    assert_eq!(
        loaded
            .document()
            .toc()
            .iter()
            .map(|entry| (entry.label(), entry.depth()))
            .collect::<Vec<_>>(),
        vec![("第一章 风起", 0), ("第二章 云涌", 0)]
    );
    assert_jump_shows(&loaded, "第二章 云涌", "第二章 云涌");
}

#[test]
fn misleading_fragment_is_repaired_by_the_heading_in_its_declared_file() {
    let loaded = open_test_epub(&[
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/book.opf",
            r#"<package><metadata><title>Fragment fixture</title></metadata><manifest>
                 <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
                 <item id="front" href="front.xhtml" media-type="application/xhtml+xml"/>
                 <item id="chapters" href="chapters.xhtml" media-type="application/xhtml+xml"/>
               </manifest><spine toc="ncx"><itemref idref="front"/><itemref idref="chapters"/></spine></package>"#
                .to_owned(),
        ),
        (
            "OPS/toc.ncx",
            r#"<ncx><navMap>
                 <navPoint><navLabel><text>目录</text></navLabel><content src="front.xhtml#directory"/></navPoint>
                 <navPoint><navLabel><text>第一章 春</text></navLabel><content src="chapters.xhtml#two"/></navPoint>
                 <navPoint><navLabel><text>第二章 夏</text></navLabel><content src="chapters.xhtml#two"/></navPoint>
               </navMap></ncx>"#
                .to_owned(),
        ),
        (
            "OPS/front.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="directory">目录</h1><p>章节集合。</p></body></html>"#
                .to_owned(),
        ),
        (
            "OPS/chapters.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
                 <h1 id="one">第一章 春</h1><p>春。</p>
                 <h1 id="two">第二章 夏</h1><p>夏。</p>
               </body></html>"#
                .to_owned(),
        ),
    ]);

    assert_jump_shows(&loaded, "目录", "目录");
    assert_jump_shows(&loaded, "第一章 春", "第一章 春");
    assert_jump_shows(&loaded, "第二章 夏", "第二章 夏");
    assert!(loaded.document().toc().windows(2).all(|pair| pair[0].offset() <= pair[1].offset()));
}

#[test]
fn front_and_tail_chapter_collections_do_not_steal_toc_targets() {
    let loaded = open_test_epub(&[
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/book.opf",
            r#"<package><metadata><title>Repeated collection fixture</title></metadata><manifest>
                 <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
                 <item id="book" href="book.xhtml" media-type="application/xhtml+xml"/>
               </manifest><spine toc="ncx"><itemref idref="book"/></spine></package>"#
                .to_owned(),
        ),
        (
            "OPS/toc.ncx",
            r#"<ncx><navMap>
                 <navPoint><navLabel><text>第一章 花</text></navLabel><content src="book.xhtml#missing-one"/></navPoint>
                 <navPoint><navLabel><text>第二章 月</text></navLabel><content src="book.xhtml#missing-two"/></navPoint>
               </navMap></ncx>"#
                .to_owned(),
        ),
        (
            "OPS/book.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
                 <div class="front-contents"><p>第一章 花</p><p>第二章 月</p></div>
                 <h1 id="one">第一章 花</h1><p>第一章正文。</p>
                 <h1 id="two">第二章 月</h1><p>第二章正文。</p>
                 <div class="tail-contents"><p>第一章 花</p><p>第二章 月</p></div>
               </body></html>"#
                .to_owned(),
        ),
    ]);

    assert_jump_shows(&loaded, "第一章 花", "第一章正文。");
    assert_jump_shows(&loaded, "第二章 月", "第二章正文。");
    assert!(loaded.document().toc()[0].offset() < loaded.document().toc()[1].offset());
}

#[test]
fn txt_chapters_use_strict_line_start_grammar_and_preserve_volume_hierarchy() {
    let text = concat!(
        "第二回合开始后，众人都沉默了。\n",
        "第二部长篇小说并不是卷名。\n",
        "他说到第二回往事时停了下来。\n",
        "第一卷　最后一战　第一章　心事一灯知\n",
        "正文。\n",
        "第一卷　最后一战　第二章　痛作无家别\n",
        "正文。\n",
        "第1357章　昨日\n",
        "正文。\n",
        "第1357章　明日\n",
        "正文。\n",
    );
    let mut file = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    file.write_all(text.as_bytes()).unwrap();
    let loaded =
        open_document(DocumentSource::from_path(file.path()), LoadOptions::default()).unwrap();
    assert_canonical_toc(&loaded);

    let toc = loaded.document().toc();
    assert_eq!(
        toc.iter().map(|entry| (entry.label(), entry.depth())).collect::<Vec<_>>(),
        vec![
            ("第一卷　最后一战", 0),
            ("第一章　心事一灯知", 1),
            ("第二章　痛作无家别", 1),
            ("第1357章　昨日", 1),
            ("第1357章　明日", 1),
        ]
    );
    assert_eq!(toc[0].offset(), toc[1].offset());
    assert_jump_shows(&loaded, "第二章　痛作无家别", "第二章　痛作无家别");
}

#[test]
fn wrong_ncx_paths_are_repaired_by_globally_unique_semantic_headings() {
    let loaded = open_test_epub(&[
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/book.opf",
            r#"<package><metadata><title>Wrong path fixture</title></metadata><manifest>
                 <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
                 <item id="one" href="one.xhtml" media-type="application/xhtml+xml"/>
                 <item id="two" href="two.xhtml" media-type="application/xhtml+xml"/>
                 <item id="three" href="three.xhtml" media-type="application/xhtml+xml"/>
               </manifest><spine toc="ncx">
                 <itemref idref="one"/><itemref idref="two"/><itemref idref="three"/>
               </spine></package>"#
                .to_owned(),
        ),
        (
            "OPS/toc.ncx",
            r#"<ncx><navMap>
                 <navPoint><navLabel><text>第一章 松</text></navLabel><content src="one.xhtml#missing"/></navPoint>
                 <navPoint><navLabel><text>第二章 竹</text></navLabel><content src="one.xhtml#missing"/></navPoint>
                 <navPoint><navLabel><text>第三章 梅</text></navLabel><content src="one.xhtml#missing"/></navPoint>
               </navMap></ncx>"#
                .to_owned(),
        ),
        ("OPS/one.xhtml", chapter_xhtml("one", "第一章 松")),
        ("OPS/two.xhtml", chapter_xhtml("two", "第二章 竹")),
        ("OPS/three.xhtml", chapter_xhtml("three", "第三章 梅")),
    ]);

    for label in ["第一章 松", "第二章 竹", "第三章 梅"] {
        assert_jump_shows(&loaded, label, label);
    }
    assert!(loaded.document().toc().windows(2).all(|pair| pair[0].offset() < pair[1].offset()));
}

#[test]
fn nested_epub_nav_is_preferred_and_tail_navigation_is_not_rendered_as_prose() {
    let loaded = open_test_epub(&[
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/book.opf",
            r#"<package><metadata><title>Nav fixture</title></metadata><manifest>
                 <item id="book" href="book.xhtml" media-type="application/xhtml+xml" properties="nav"/>
                 <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
               </manifest><spine toc="ncx"><itemref idref="book"/></spine></package>"#
                .to_owned(),
        ),
        (
            "OPS/toc.ncx",
            r#"<ncx><navMap><navPoint><navLabel><text>开始</text></navLabel><content src="book.xhtml"/></navPoint></navMap></ncx>"#
                .to_owned(),
        ),
        (
            "OPS/book.xhtml",
            r##"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
                 <h1 id="part">第一卷 山河</h1>
                 <h2 id="chapter">第一章 入城</h2><p>正文。</p>
                 <nav epub:type="toc"><p>尾部目录独有文本</p><ol>
                   <li><a href="#part">第一卷 山河</a><ol>
                     <li><a href="#chapter">第一章 入城</a></li>
                   </ol></li>
                 </ol></nav>
               </body></html>"##
                .to_owned(),
        ),
    ]);

    assert!(!loaded.document().text().contains("尾部目录独有文本"));
    assert_eq!(
        loaded
            .document()
            .toc()
            .iter()
            .map(|entry| (entry.label(), entry.depth()))
            .collect::<Vec<_>>(),
        vec![("第一卷 山河", 0), ("第一章 入城", 1)]
    );
    assert_jump_shows(&loaded, "第一章 入城", "第一章 入城");
}

#[test]
fn non_chapter_landmarks_use_their_declared_section_or_anchor() {
    let loaded = open_test_epub(&[
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/book.opf",
            r#"<package><metadata><title>Landmark fixture</title></metadata><manifest>
                 <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
                 <item id="directory" href="directory.xhtml" media-type="application/xhtml+xml"/>
                 <item id="rights" href="rights.xhtml" media-type="application/xhtml+xml"/>
                 <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
               </manifest><spine toc="ncx">
                 <itemref idref="directory"/><itemref idref="rights"/><itemref idref="chapter"/>
               </spine></package>"#
                .to_owned(),
        ),
        (
            "OPS/toc.ncx",
            r#"<ncx><navMap>
                 <navPoint><navLabel><text>目录</text></navLabel><content src="directory.xhtml"/></navPoint>
                 <navPoint><navLabel><text>版权页</text></navLabel><content src="rights.xhtml#rights"/></navPoint>
                 <navPoint><navLabel><text>第一章 正文</text></navLabel><content src="chapter.xhtml#chapter"/></navPoint>
               </navMap></ncx>"#
                .to_owned(),
        ),
        (
            "OPS/directory.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>示例书名</h1><p>第一章 正文</p></body></html>"#
                .to_owned(),
        ),
        (
            "OPS/rights.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><div id="rights"><h1>示例书名</h1><p>作者与出版信息。</p></div></body></html>"#
                .to_owned(),
        ),
        ("OPS/chapter.xhtml", chapter_xhtml("chapter", "第一章 正文")),
    ]);

    assert_eq!(
        loaded.document().toc().iter().map(|entry| entry.label()).collect::<Vec<_>>(),
        vec!["目录", "版权页", "第一章 正文"]
    );
    assert_jump_shows(&loaded, "目录", "示例书名");
    assert_jump_shows(&loaded, "版权页", "示例书名");
}

#[test]
fn epub3_nav_with_self_closing_script_is_parsed_as_xhtml() {
    let loaded = open_test_epub(&[
        ("META-INF/container.xml", container_xml()),
        (
            "OPS/book.opf",
            r#"<package><metadata><title>XHTML nav fixture</title></metadata><manifest>
                 <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
                 <item id="book" href="book.xhtml" media-type="application/xhtml+xml"/>
               </manifest><spine><itemref idref="book"/></spine></package>"#
                .to_owned(),
        ),
        (
            "OPS/nav.xhtml",
            r##"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
                 <head><script src="reader.js"/></head><body><nav epub:type="toc"><ol>
                   <li><a href="book.xhtml#part">第一部 起点</a><ol>
                     <li><a href="book.xhtml#chapter">第一章 出发</a></li>
                   </ol></li>
                 </ol></nav></body>
               </html>"##
                .to_owned(),
        ),
        (
            "OPS/book.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
                 <h1 id="part">第一部 起点</h1><h2 id="chapter">第一章 出发</h2><p>正文。</p>
               </body></html>"#
                .to_owned(),
        ),
    ]);

    assert_eq!(
        loaded
            .document()
            .toc()
            .iter()
            .map(|entry| (entry.label(), entry.depth()))
            .collect::<Vec<_>>(),
        vec![("第一部 起点", 0), ("第一章 出发", 1)]
    );
}

fn assert_jump_shows(loaded: &peep::document::LoadedDocument, label: &str, expected: &str) {
    let entry = loaded.document().toc().iter().find(|entry| entry.label() == label).unwrap();
    let text = loaded.document().text();
    let mut viewport = Viewport::new(text, 0);
    viewport.set_width(80);
    viewport.goto_byte(text, entry.offset());
    let visible = viewport
        .visible_lines(text, 3)
        .into_iter()
        .map(|line| &text[line.range()])
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains(expected), "jump for {label:?} showed {visible:?}");
}

fn assert_canonical_toc(loaded: &peep::document::LoadedDocument) {
    let text = loaded.document().text();
    let toc = loaded.document().toc();
    assert!(toc.windows(2).all(|pair| pair[0].offset() <= pair[1].offset()));
    for entry in toc {
        assert!(entry.offset() < text.len());
        assert!(text.is_char_boundary(entry.offset()));
        let mut viewport = Viewport::new(text, 0);
        viewport.set_width(80);
        viewport.goto_byte(text, entry.offset());
        assert!(viewport.anchor() <= entry.offset());
    }
    for pair in toc.windows(2).filter(|pair| pair[0].offset() == pair[1].offset()) {
        assert!(
            pair[0].depth() != pair[1].depth() || pair[0].label() == pair[1].label(),
            "unrelated TOC entries share offset {}: {:?} and {:?}",
            pair[0].offset(),
            pair[0].label(),
            pair[1].label()
        );
    }
}

fn open_test_epub(entries: &[(&str, String)]) -> peep::document::LoadedDocument {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, contents) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&bytes).unwrap();
    let loaded =
        open_document(DocumentSource::from_path(file.path()), LoadOptions::default()).unwrap();
    assert_canonical_toc(&loaded);
    loaded
}

fn container_xml() -> String {
    r#"<?xml version="1.0"?>
       <container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#
        .to_owned()
}

fn package_xml(manifest: &str, spine: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
           <package>
             <metadata><title>Recovery fixture</title></metadata>
             <manifest>{manifest}</manifest>
             <spine>{spine}</spine>
           </package>"#
    )
}

fn chapter_xhtml(id: &str, label: &str) -> String {
    format!(
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="{id}">{label}</h1><p>正文。</p></body></html>"#
    )
}
