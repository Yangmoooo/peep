use std::ops::Range;

use directories::UserDirs;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
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
use crate::terminal_palette::DefaultColors;
use crate::viewport::VisualLine;

const MAX_BODY_WIDTH: u16 = 100;

#[derive(Clone, Copy)]
struct UiPalette {
    surface: Color,
    text: Color,
    muted: Color,
    accent: Color,
    has_surface: bool,
}

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    if area.width < 20 || area.height < 6 {
        frame.render_widget(
            Paragraph::new("Terminal too small")
                .style(Style::default().fg(Color::DarkGray))
                .centered(),
            area,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3), Constraint::Length(1)])
        .split(area);

    render_body(frame, app, rows[0]);
    render_composer(frame, app, rows[1]);
    render_status(frame, app, rows[2]);
    if app.overlay().is_some() {
        render_overlay(frame, app, rows[0]);
    }
}

fn render_body(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
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
            .map(|line| styled_line(loaded.document(), line, app.current_match()))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    frame.render_widget(Paragraph::new(Text::from(lines)), body);
}

fn render_composer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let area = area.inner(Margin { horizontal: 1, vertical: 0 });
    let palette = ui_palette();
    let surface_style = Style::default().fg(palette.text).bg(palette.surface);
    let text = app.composer_text();
    let prompt_style = match app.input_mode() {
        InputMode::Command | InputMode::Search => {
            surface_style.fg(palette.accent).add_modifier(Modifier::BOLD)
        }
        InputMode::Normal => surface_style.fg(palette.muted),
    };
    let text_style = match app.input_mode() {
        InputMode::Command | InputMode::Search => surface_style,
        InputMode::Normal if app.loading_path.is_some() => surface_style.fg(palette.accent),
        InputMode::Normal if app.message.is_some() => surface_style,
        InputMode::Normal => surface_style.fg(palette.muted),
    };
    let line =
        Line::from(vec![Span::styled("› ", prompt_style), Span::styled(text.as_str(), text_style)]);
    let block = if palette.has_surface {
        Block::default().padding(ratatui::widgets::Padding::new(2, 1, 1, 1))
    } else {
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(palette.muted))
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

fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let area = area.inner(Margin { horizontal: 3, vertical: 0 });
    let left = compact_home(app.cwd());
    let right = format!("{:.0}%", app.progress_percent());
    let line = fit_status(&left, &right, area.width as usize);
    frame.render_widget(Paragraph::new(line).style(Style::default().fg(ui_palette().muted)), area);
}

fn render_overlay(frame: &mut Frame<'_>, app: &mut App, body: Rect) {
    let Some(kind) = app.overlay().map(|overlay| overlay.kind) else {
        return;
    };
    let items = app.overlay_items();
    let desired_height = (items.len().min(u16::MAX as usize) as u16).saturating_add(2).max(3);
    let width = body.width.saturating_sub(4).clamp(10, 80);
    let height = desired_height.min(body.height.saturating_sub(1).max(3));
    let area = centered_rect(width, height, body);
    let palette = ui_palette();
    let surface_style = Style::default().fg(palette.text).bg(palette.surface);
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
            .title_style(Style::default().fg(palette.accent))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(palette.muted))
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
                .track_style(Style::default().fg(palette.muted).bg(palette.surface))
                .thumb_style(Style::default().fg(palette.accent).bg(palette.surface));
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
                Span::styled(
                    matched,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(after),
            ]))
        })
        .collect::<Vec<_>>();
    let list = List::new(list_items)
        .style(surface_style)
        .block(overlay_block())
        .highlight_style(
            Style::default().fg(Color::Black).bg(palette.accent).add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");

    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn styled_line<'a>(
    document: &'a CanonicalDocument,
    visual: &VisualLine,
    current_match: Option<Range<usize>>,
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
                let mut style = Style::default();
                for text_style in document.styles() {
                    let style_range = text_style.range();
                    if start >= style_range.start && start < style_range.end {
                        style = match text_style.kind() {
                            TextStyleKind::Emphasis => style.add_modifier(Modifier::ITALIC),
                            TextStyleKind::Strong => style.add_modifier(Modifier::BOLD),
                            TextStyleKind::Heading(_) => {
                                style.fg(Color::Cyan).add_modifier(Modifier::BOLD)
                            }
                        };
                    }
                }
                if current_match
                    .as_ref()
                    .is_some_and(|found| start >= found.start && start < found.end)
                {
                    style = style.fg(Color::Black).bg(Color::Yellow);
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

fn ui_palette() -> UiPalette { palette_for_default_colors(terminal_palette::default_colors()) }

fn palette_for_default_colors(colors: Option<DefaultColors>) -> UiPalette {
    let Some(colors) = colors else {
        return UiPalette {
            surface: Color::Reset,
            text: Color::Reset,
            muted: Color::DarkGray,
            accent: Color::Cyan,
            has_surface: false,
        };
    };
    let light = is_light(colors.background);
    let surface = if light {
        blend((0, 0, 0), colors.background, 0.04)
    } else {
        blend((255, 255, 255), colors.background, 0.12)
    };
    UiPalette {
        surface: rgb(surface),
        text: rgb(colors.foreground),
        muted: rgb(blend(colors.foreground, colors.background, 0.55)),
        accent: if light { Color::Rgb(0, 95, 135) } else { Color::Cyan },
        has_surface: true,
    }
}

fn is_light((red, green, blue): (u8, u8, u8)) -> bool {
    299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue) > 128_000
}

fn blend(top: (u8, u8, u8), bottom: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    let channel =
        |top: u8, bottom: u8| (f32::from(top) * alpha + f32::from(bottom) * (1.0 - alpha)) as u8;
    (channel(top.0, bottom.0), channel(top.1, bottom.1), channel(top.2, bottom.2))
}

fn rgb((red, green, blue): (u8, u8, u8)) -> Color { Color::Rgb(red, green, blue) }

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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
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
        assert_eq!(
            terminal.backend().buffer().cell(Position::new(1, 6)).unwrap().bg,
            ui_palette().surface
        );
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
    fn derives_low_contrast_surfaces_from_terminal_colors() {
        let light = palette_for_default_colors(Some(DefaultColors {
            foreground: (0, 0, 0),
            background: (255, 255, 255),
        }));
        assert_eq!(light.surface, Color::Rgb(244, 244, 244));
        assert!(light.has_surface);

        let dark = palette_for_default_colors(Some(DefaultColors {
            foreground: (255, 255, 255),
            background: (0, 0, 0),
        }));
        assert_eq!(dark.surface, Color::Rgb(30, 30, 30));
        assert!(dark.has_surface);

        let unknown = palette_for_default_colors(None);
        assert_eq!(unknown.surface, Color::Reset);
        assert!(!unknown.has_surface);
    }

    #[test]
    fn overlay_wrapping_counts_cjk_terminal_width() {
        let rows = wrap_overlay_items(&["中文测试".to_owned()], 4);
        assert_eq!(rows, ["中文", "测试"]);
    }
}
