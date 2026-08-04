use std::ops::Range;

use directories::UserDirs;
use ratatui::Frame;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar,
    ScrollbarOrientation, ScrollbarState,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, InputMode};
use crate::document::{CanonicalDocument, TextStyleKind};
use crate::terminal_palette;
use crate::theme::Theme;
use crate::viewport::VisualLine;

const MAX_BODY_WIDTH: u16 = 100;

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let theme = Theme::resolve(app.theme_choice(), terminal_palette::default_colors());
    frame.render_widget(Block::default().style(theme.body), area);
    if area.width < 20 || area.height < 6 {
        frame.render_widget(
            Paragraph::new("Terminal too small").style(theme.muted).centered(),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3), Constraint::Length(1)])
        .split(area);

    render_body(frame, app, rows[0], theme);
    render_composer(frame, app, rows[1], theme);
    render_status(frame, app, rows[2], theme);
    if app.overlay().is_some() {
        render_overlay(frame, app, rows[0], theme);
    }
}

fn render_body(frame: &mut Frame<'_>, app: &mut App, area: Rect, theme: Theme) {
    let width = area.width.saturating_sub(4).clamp(1, MAX_BODY_WIDTH);
    let body = Rect::new(
        area.x.saturating_add(2),
        area.y,
        width.min(area.width.saturating_sub(2)),
        area.height,
    );
    let visual_lines = app.visible_lines(body.width as usize, body.height as usize);
    let lines = if let Some(loaded) = app.document() {
        visual_lines
            .iter()
            .map(|line| styled_line(loaded.document(), line, app.current_match(), theme))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    frame.render_widget(Paragraph::new(Text::from(lines)).style(theme.body), body);
}

fn render_composer(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let area = area.inner(Margin { horizontal: 1, vertical: 0 });
    let surface_style = theme.surface;
    let text = app.composer_text();
    let prompt_style = match app.input_mode() {
        InputMode::Command | InputMode::Search | InputMode::Filter => {
            surface_style.patch(theme.accent).add_modifier(Modifier::BOLD)
        }
        InputMode::Normal => surface_style.patch(theme.muted),
    };
    let text_style = match app.input_mode() {
        InputMode::Command | InputMode::Search | InputMode::Filter => surface_style,
        InputMode::Normal if app.loading_path.is_some() => surface_style.patch(theme.accent),
        InputMode::Normal if app.message.is_some() => surface_style,
        InputMode::Normal => surface_style.patch(theme.muted),
    };
    let line =
        Line::from(vec![Span::styled("› ", prompt_style), Span::styled(text.as_str(), text_style)]);
    let block = if theme.surface_visible {
        Block::default().padding(ratatui::widgets::Padding::new(2, 1, 1, 1))
    } else {
        Block::default()
            .borders(Borders::TOP)
            .border_style(theme.border)
            .padding(ratatui::widgets::Padding::new(2, 1, 0, 1))
    };
    let surface = Paragraph::new(line).style(surface_style).block(block);
    frame.render_widget(surface, area);

    if app.input_mode() != InputMode::Normal {
        let prompt_width = 4 + UnicodeWidthStr::width(text.as_str());
        let max_x = area.right().saturating_sub(1);
        let x = area.x.saturating_add(prompt_width.min(u16::MAX as usize) as u16).min(max_x);
        frame.set_cursor_position(Position::new(x, area.y.saturating_add(1)));
    }
}

fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect, theme: Theme) {
    let area = area.inner(Margin { horizontal: 3, vertical: 0 });
    let left = compact_home(app.cwd());
    let right = format!("{:.0}%", app.progress_percent());
    let line = fit_status(&left, &right, area.width as usize);
    frame.render_widget(Paragraph::new(line).style(theme.body.patch(theme.muted)), area);
}

fn render_overlay(frame: &mut Frame<'_>, app: &mut App, body: Rect, theme: Theme) {
    let Some(kind) = app.overlay().map(|overlay| overlay.kind) else {
        return;
    };
    let items = app.overlay_items();
    let desired_height = (items.len().min(u16::MAX as usize) as u16).saturating_add(2).max(3);
    let width = body.width.saturating_sub(4).clamp(10, 80);
    let height = desired_height.min(body.height.saturating_sub(1).max(3));
    let area = centered_rect(width, height, body);
    let surface_style = theme.surface;
    frame.render_widget(Clear, area);

    let wrapped_items = (!kind.is_list())
        .then(|| wrap_overlay_items(&items, area.width.saturating_sub(2) as usize));
    let total_rows = wrapped_items.as_ref().map_or(items.len(), Vec::len);
    let viewport_rows = area.height.saturating_sub(2) as usize;
    app.set_overlay_layout(total_rows, viewport_rows);
    let title = app.overlay_title().unwrap_or_default();
    let overlay_block = || {
        Block::default()
            .title(title.as_str())
            .title_style(surface_style.patch(theme.accent))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border)
            .style(surface_style)
    };

    if let Some(content_rows) = wrapped_items {
        let scroll =
            app.overlay().map_or(0, |overlay| overlay.selected).min(u16::MAX as usize) as u16;
        let content = Text::from(content_rows.into_iter().map(Line::from).collect::<Vec<_>>());
        let paragraph =
            Paragraph::new(content).style(surface_style).scroll((scroll, 0)).block(overlay_block());
        frame.render_widget(paragraph, area);

        if total_rows > viewport_rows {
            let mut state = ScrollbarState::new(total_rows)
                .position(scroll as usize)
                .viewport_content_length(viewport_rows);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_symbol("┃")
                .track_style(theme.border)
                .thumb_style(surface_style.patch(theme.accent));
            frame.render_stateful_widget(
                scrollbar,
                area.inner(Margin { horizontal: 0, vertical: 1 }),
                &mut state,
            );
        }
        return;
    }

    let selected = app.overlay().map_or(0, |overlay| overlay.selected);
    let emphasis = (0..items.len()).map(|index| app.overlay_item_emphasis(index));
    let list_items = items
        .into_iter()
        .zip(emphasis)
        .map(|(item, emphasis)| {
            let Some(range) = emphasis.filter(|range| {
                range.start < range.end
                    && range.end <= item.len()
                    && item.is_char_boundary(range.start)
                    && item.is_char_boundary(range.end)
            }) else {
                return ListItem::new(item);
            };
            let before = item[..range.start].to_owned();
            let matched = item[range.clone()].to_owned();
            let after = item[range.end..].to_owned();
            ListItem::new(Line::from(vec![
                Span::raw(before),
                Span::styled(matched, surface_style.patch(theme.matched)),
                Span::raw(after),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(list_items)
        .style(surface_style)
        .block(overlay_block())
        .highlight_style(theme.selected)
        .highlight_symbol("› ");

    let mut state =
        ListState::default().with_selected(Some(selected)).with_offset(app.overlay_list_offset());
    frame.render_stateful_widget(list, area, &mut state);
    force_redraw_selected_list_row(frame, area, selected, state.offset());
    app.set_overlay_list_offset(state.offset());
}

fn force_redraw_selected_list_row(
    frame: &mut Frame<'_>,
    area: Rect,
    selected: usize,
    offset: usize,
) {
    let list_area = area.inner(Margin { horizontal: 1, vertical: 1 });
    let Some(relative_row) = selected.checked_sub(offset) else {
        return;
    };
    let Ok(relative_row) = u16::try_from(relative_row) else {
        return;
    };
    if relative_row >= list_area.height {
        return;
    }
    let y = list_area.y.saturating_add(relative_row);
    for x in list_area.left()..list_area.right() {
        if let Some(cell) = frame.buffer_mut().cell_mut(Position::new(x, y)) {
            cell.set_diff_option(CellDiffOption::AlwaysUpdate);
        }
    }
}

fn styled_line<'a>(
    document: &'a CanonicalDocument,
    visual: &VisualLine,
    current_match: Option<Range<usize>>,
    theme: Theme,
) -> Line<'a> {
    let range = visual.range();
    if range.is_empty() {
        return Line::default();
    }
    let mut points = vec![range.start, range.end];
    for style in document.styles() {
        if intersects(&range, &style.range()) {
            points.push(style.range().start.max(range.start));
            points.push(style.range().end.min(range.end));
        }
    }
    if let Some(found) = &current_match
        && intersects(&range, found)
    {
        points.push(found.start.max(range.start));
        points.push(found.end.min(range.end));
    }
    points.sort_unstable();
    points.dedup();

    let spans = points
        .windows(2)
        .filter_map(|window| {
            let start = window[0];
            let end = window[1];
            (start < end).then(|| {
                let mut style = theme.body;
                for text_style in document.styles() {
                    let style_range = text_style.range();
                    if start >= style_range.start && start < style_range.end {
                        style = match text_style.kind() {
                            TextStyleKind::Emphasis => style.add_modifier(Modifier::ITALIC),
                            TextStyleKind::Strong => style.add_modifier(Modifier::BOLD),
                            TextStyleKind::Heading(_) => style.patch(theme.heading),
                        };
                    }
                }
                if current_match
                    .as_ref()
                    .is_some_and(|found| start >= found.start && start < found.end)
                {
                    style = style.patch(theme.matched);
                }
                Span::styled(&document.text()[start..end], style)
            })
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn intersects(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn wrap_overlay_items(items: &[String], width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for item in items {
        for hard_line in item.split('\n') {
            let mut row = String::new();
            let mut used = 0_usize;
            for grapheme in hard_line.graphemes(true) {
                let grapheme_width = UnicodeWidthStr::width(grapheme);
                if used > 0 && used.saturating_add(grapheme_width) > width {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
                row.push_str(grapheme);
                used = used.saturating_add(grapheme_width);
            }
            rows.push(row);
        }
    }
    rows
}

fn compact_home(path: &std::path::Path) -> String {
    if let Some(home) = UserDirs::new().map(|directories| directories.home_dir().to_path_buf())
        && let Ok(relative) = path.strip_prefix(home)
    {
        return if relative.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", relative.display())
        };
    }
    path.display().to_string()
}

fn fit_status(left: &str, right: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let right_width = UnicodeWidthStr::width(right);
    if right_width >= width {
        return truncate_width(right, width);
    }
    let separator = " · ";
    let separator_width = UnicodeWidthStr::width(separator);
    if right_width + separator_width >= width {
        return right.to_owned();
    }
    let available_left = width - right_width - separator_width;
    let left = truncate_width(left, available_left);
    format!("{left}{separator}{right}")
}

fn truncate_width(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let ellipsis = "…";
    let target = width.saturating_sub(UnicodeWidthStr::width(ellipsis));
    let mut result = String::new();
    let mut used = 0;
    for grapheme in value.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used + grapheme_width > target {
            break;
        }
        result.push_str(grapheme);
        used += grapheme_width;
    }
    result.push_str(ellipsis);
    result
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width.min(area.width),
        height.min(area.height),
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};
    use std::{fs, thread};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::{OverlayKind, OverlayState};
    use crate::state::StateStore;

    #[test]
    fn renders_blank_conversation_shell() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("› Ask anything…"));
        assert!(!rendered.contains('╭'));
        assert!(rendered.contains("0%"));
    }

    #[test]
    fn toc_cursor_moves_up_before_the_list_viewport_scrolls() {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        let text = (1..=20)
            .map(|number| format!("第{number}章 标题{number}\n正文。\n"))
            .collect::<String>();
        fs::write(&book, text).unwrap();
        let store = StateStore::at(directory.path().join("state")).unwrap();
        let mut app = App::new(directory.path().to_path_buf(), store);
        app.start_load(book);
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.document().is_none() && Instant::now() < deadline {
            app.poll_tasks();
            thread::yield_now();
        }
        assert_eq!(app.document().unwrap().document().toc().len(), 20);
        app.overlay = Some(OverlayState::new(OverlayKind::Toc, 19));

        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let bottom_row = selected_overlay_row(&terminal, 8);
        let title_separator = terminal
            .backend()
            .buffer()
            .cell(Position::new(11, bottom_row))
            .expect("space between chapter number and title");
        assert_eq!(title_separator.symbol(), " ");
        assert_eq!(title_separator.diff_option, CellDiffOption::AlwaysUpdate);

        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let moved_row = selected_overlay_row(&terminal, 8);

        assert_eq!(moved_row + 1, bottom_row);
    }

    fn selected_overlay_row(terminal: &Terminal<TestBackend>, body_height: u16) -> u16 {
        let buffer = terminal.backend().buffer();
        (0..body_height)
            .find(|&y| {
                (0..buffer.area.width).any(|x| {
                    buffer.cell(Position::new(x, y)).is_some_and(|cell| cell.symbol() == "›")
                })
            })
            .expect("selected overlay row")
    }

    #[test]
    fn status_truncation_preserves_progress() {
        let status = fit_status("/a/very/long/path", "38%", 10);
        assert!(UnicodeWidthStr::width(status.as_str()) <= 10);
        assert!(status.ends_with("38%"));
        assert!(!status.contains("  "));
        assert_eq!(fit_status("~/repo", "38%", 80), "~/repo · 38%");
    }

    #[test]
    fn overlay_wrapping_counts_cjk_terminal_width() {
        let rows = wrap_overlay_items(&["中文测试".to_owned()], 4);
        assert_eq!(rows, ["中文", "测试"]);
    }
}
