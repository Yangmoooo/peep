use std::cmp::Ordering;

use percent_encoding::percent_decode_str;
use scraper::node::Node;
use scraper::{Html, Selector};

use super::super::toc_label_is_landmark;
use super::{
    AssembledDocument, TocEntry, archive_parent, collapse_whitespace, detect_chapter_headings,
    normalise_archive_name, normalise_relative_archive_path,
};

#[derive(Clone, Debug)]
pub(super) struct RawTocEntry {
    label: String,
    depth: u8,
    target: EpubTarget,
}

#[derive(Clone, Debug)]
struct EpubTarget {
    path: String,
    fragment: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct ResolvedToc {
    entries: Vec<TocEntry>,
    raw_count: usize,
}

pub(super) fn parse_nav(source: &str, nav_path: &str) -> Vec<RawTocEntry> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: 1_000_000,
        entity_resolver: None,
    };
    if let Ok(document) = roxmltree::Document::parse_with_options(source, options) {
        return parse_xml_nav(&document, nav_path);
    }

    let document = Html::parse_document(source);
    let nav_selector = Selector::parse("nav").expect("static selector");
    let link_selector = Selector::parse("a[href]").expect("static selector");
    let nav = document
        .select(&nav_selector)
        .find(|element| {
            element.value().attrs().any(|(name, value)| {
                name.ends_with("type") && value.split_whitespace().any(|part| part == "toc")
            })
        })
        .or_else(|| document.select(&nav_selector).next());
    let Some(nav) = nav else {
        return Vec::new();
    };

    nav.select(&link_selector)
        .filter_map(|link| {
            let label = collapse_whitespace(&link.text().collect::<String>());
            if label.is_empty() {
                return None;
            }
            let target = parse_target(nav_path, link.value().attr("href")?)?;
            let depth = link
                .ancestors()
                .filter(|ancestor| {
                    matches!(ancestor.value(), Node::Element(element) if element.name() == "li")
                })
                .count()
                .saturating_sub(1)
                .min(u8::MAX as usize) as u8;
            Some(RawTocEntry { label, depth, target })
        })
        .collect()
}

fn parse_xml_nav(document: &roxmltree::Document<'_>, nav_path: &str) -> Vec<RawTocEntry> {
    let nav = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "nav")
        .find(|node| {
            node.attributes().any(|attribute| {
                attribute.name().ends_with("type")
                    && attribute.value().split_whitespace().any(|part| part == "toc")
            })
        })
        .or_else(|| {
            document.descendants().find(|node| node.is_element() && node.tag_name().name() == "nav")
        });
    let Some(nav) = nav else {
        return Vec::new();
    };

    nav.descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "a")
        .filter_map(|link| {
            let label = collapse_whitespace(
                &link
                    .descendants()
                    .filter(|node| node.is_text())
                    .filter_map(|node| node.text())
                    .collect::<String>(),
            );
            if label.is_empty() {
                return None;
            }
            let target = parse_target(nav_path, link.attribute("href")?)?;
            let depth = link
                .ancestors()
                .filter(|ancestor| ancestor.is_element() && ancestor.tag_name().name() == "li")
                .count()
                .saturating_sub(1)
                .min(u8::MAX as usize) as u8;
            Some(RawTocEntry { label, depth, target })
        })
        .collect()
}

pub(super) fn parse_ncx(source: &str, ncx_path: &str) -> Vec<RawTocEntry> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: 1_000_000,
        entity_resolver: None,
    };
    let Ok(document) = roxmltree::Document::parse_with_options(source, options) else {
        return Vec::new();
    };
    document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "navPoint")
        .filter_map(|point| {
            let label = point
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "text")?
                .text()
                .map(collapse_whitespace)?;
            if label.is_empty() {
                return None;
            }
            let href = point
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "content")?
                .attribute("src")?;
            let target = parse_target(ncx_path, href)?;
            let depth = point
                .ancestors()
                .filter(|node| node.is_element() && node.tag_name().name() == "navPoint")
                .count()
                .saturating_sub(1)
                .min(u8::MAX as usize) as u8;
            Some(RawTocEntry { label, depth, target })
        })
        .collect()
}

pub(super) fn resolve(raw: Vec<RawTocEntry>, assembled: &AssembledDocument) -> ResolvedToc {
    let raw_count = raw.len();
    let detected = detect_chapter_headings(&assembled.text);
    let mut resolved = Vec::new();
    for entry in raw {
        if let Some(offset) = resolve_entry(&entry, assembled, &detected) {
            resolved.push(TocEntry { label: entry.label, offset, depth: entry.depth });
        }
    }

    let entries = longest_monotonic_subsequence(resolved).into_iter().fold(
        Vec::<TocEntry>::new(),
        |mut entries, entry| {
            let duplicate_is_meaningful = entries.last().is_some_and(|previous| {
                previous.offset == entry.offset
                    && (previous.depth != entry.depth
                        || labels_compatible(&previous.label, &entry.label))
            });
            if entries
                .last()
                .is_none_or(|previous| previous.offset != entry.offset || duplicate_is_meaningful)
            {
                entries.push(entry);
            }
            entries
        },
    );
    ResolvedToc { entries, raw_count }
}

pub(super) fn choose_best(candidates: impl IntoIterator<Item = ResolvedToc>) -> Vec<TocEntry> {
    candidates
        .into_iter()
        .filter(|candidate| !candidate.entries.is_empty())
        .max_by(compare_quality)
        .map_or_else(Vec::new, |candidate| candidate.entries)
}

fn compare_quality(left: &ResolvedToc, right: &ResolvedToc) -> Ordering {
    let left_coverage = coverage(left);
    let right_coverage = coverage(right);
    let left_usable = left_coverage >= 70;
    let right_usable = right_coverage >= 70;
    left_usable
        .cmp(&right_usable)
        .then_with(|| left.entries.len().cmp(&right.entries.len()))
        .then_with(|| left_coverage.cmp(&right_coverage))
        .then_with(|| {
            left.entries
                .iter()
                .filter(|entry| entry.depth > 0)
                .count()
                .cmp(&right.entries.iter().filter(|entry| entry.depth > 0).count())
        })
}

fn coverage(candidate: &ResolvedToc) -> usize {
    candidate.entries.len().saturating_mul(100) / candidate.raw_count.max(1)
}

fn resolve_entry(
    entry: &RawTocEntry,
    assembled: &AssembledDocument,
    detected: &[TocEntry],
) -> Option<usize> {
    let section =
        assembled.rendered_sections.iter().find(|section| section.path == entry.target.path);

    if let (Some(section), Some(fragment)) = (section, entry.target.fragment.as_deref())
        && let Some(&offset) = section.anchors.get(fragment)
        && target_matches_label(assembled, section, offset, &entry.label)
    {
        return Some(offset);
    }

    if let Some(section) = section {
        let matches = section
            .headings
            .iter()
            .filter(|heading| labels_compatible(&entry.label, &heading.label))
            .map(|heading| heading.offset)
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return matches.first().copied();
        }
    }

    let global_headings = assembled
        .rendered_sections
        .iter()
        .flat_map(|section| &section.headings)
        .filter(|heading| labels_compatible(&entry.label, &heading.label))
        .map(|heading| heading.offset)
        .collect::<Vec<_>>();
    if global_headings.len() == 1 {
        return global_headings.first().copied();
    }

    if toc_label_is_landmark(&entry.label)
        && let Some(section) = section
    {
        return entry
            .target
            .fragment
            .as_deref()
            .and_then(|fragment| section.anchors.get(fragment).copied())
            .or(Some(section.range.start));
    }

    if let Some(section) = section {
        let matches = detected
            .iter()
            .filter(|candidate| {
                section.range.contains(&candidate.offset)
                    && labels_compatible(&entry.label, &candidate.label)
            })
            .map(|candidate| candidate.offset)
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return matches.first().copied();
        }
    }

    let global_detected = detected
        .iter()
        .filter(|candidate| labels_compatible(&entry.label, &candidate.label))
        .map(|candidate| candidate.offset)
        .collect::<Vec<_>>();
    (global_detected.len() == 1).then(|| global_detected[0])
}

fn target_matches_label(
    assembled: &AssembledDocument,
    section: &super::RenderedSection,
    offset: usize,
    label: &str,
) -> bool {
    let sample = assembled.text[offset..section.range.end].chars().take(128).collect::<String>();
    if sample
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| labels_compatible(label, line))
    {
        return true;
    }
    if let Some(nearest_distance) =
        section.headings.iter().map(|heading| heading.offset.abs_diff(offset)).min()
        && nearest_distance <= 128
    {
        return section.headings.iter().any(|heading| {
            heading.offset.abs_diff(offset) == nearest_distance
                && labels_compatible(label, &heading.label)
        });
    }
    false
}

fn labels_compatible(left: &str, right: &str) -> bool {
    let left = semantic_key(left);
    let right = semantic_key(right);
    left == right
        || (left.chars().count() >= 2 && right.starts_with(&left))
        || (right.chars().count() >= 2 && left.starts_with(&right))
}

fn semantic_key(label: &str) -> String {
    label
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn parse_target(document_path: &str, href: &str) -> Option<EpubTarget> {
    let (path_with_query, fragment) =
        href.split_once('#').map_or((href, None), |(path, fragment)| {
            (path, (!fragment.is_empty()).then_some(fragment))
        });
    let path = path_with_query.split('?').next().unwrap_or_default();
    let path = if path.is_empty() {
        normalise_archive_name(document_path)
    } else {
        normalise_relative_archive_path(archive_parent(document_path), path)?
    };
    let fragment = fragment.map(|value| percent_decode_str(value).decode_utf8_lossy().into_owned());
    Some(EpubTarget { path, fragment })
}

fn longest_monotonic_subsequence(entries: Vec<TocEntry>) -> Vec<TocEntry> {
    if entries.len() <= 1 {
        return entries;
    }
    let mut tail_offsets = Vec::<usize>::new();
    let mut tail_indices = Vec::<usize>::new();
    let mut predecessors = vec![None; entries.len()];

    for (index, entry) in entries.iter().enumerate() {
        let position = tail_offsets.partition_point(|offset| *offset <= entry.offset);
        if position > 0 {
            predecessors[index] = Some(tail_indices[position - 1]);
        }
        if position == tail_offsets.len() {
            tail_offsets.push(entry.offset);
            tail_indices.push(index);
        } else {
            tail_offsets[position] = entry.offset;
            tail_indices[position] = index;
        }
    }

    let mut selected = Vec::with_capacity(tail_offsets.len());
    let mut cursor = tail_indices.last().copied();
    while let Some(index) = cursor {
        selected.push(index);
        cursor = predecessors[index];
    }
    selected.reverse();
    selected.into_iter().map(|index| entries[index].clone()).collect()
}
