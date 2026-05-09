use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use syntect::highlighting::Color as SyntectColor;

use crate::app::{App, DiffRowKind, Focus, Mode, ViewMode};

const GUTTER: usize = 9; // "▶ " (2) + "1234 " (5) + "● " (2)

pub fn render(f: &mut Frame, app: &App) {
    let has_edit = matches!(app.mode, Mode::EditComment);
    let edit_height = if has_edit {
        comment_editor_height(&app.input, f.area().width)
    } else {
        0
    };

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(edit_height),
            Constraint::Length(1),
        ])
        .split(f.area());

    let tree_pct = app.tree_width_pct;
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(tree_pct),
            Constraint::Percentage(100 - tree_pct),
        ])
        .split(outer[0]);

    render_tree(f, app, top[0]);
    render_file(f, app, top[1]);

    if has_edit {
        render_comment_edit(f, app, outer[1]);
    }

    render_status(f, app, outer[2]);
}

fn render_tree(f: &mut Frame, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let focused = matches!(app.focus, Focus::Tree);

    let lines: Vec<Line> = app
        .tree
        .entries
        .iter()
        .enumerate()
        .skip(app.tree.scroll)
        .take(inner_height)
        .map(|(i, entry)| {
            let is_selected = i == app.tree.cursor;
            let indent = "  ".repeat(entry.depth);
            let prefix = if entry.is_dir {
                if entry.is_expanded { "▼ " } else { "▶ " }
            } else {
                "  "
            };
            let name = entry
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            let suffix = if entry.is_dir { "/" } else { "" };
            let text = format!("{}{}{}{}", indent, prefix, name, suffix);

            let style = if is_selected && focused {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if is_selected {
                Style::default().add_modifier(Modifier::UNDERLINED)
            } else if entry.path.to_string_lossy() == app.file_path {
                Style::default().fg(Color::Cyan)
            } else if entry.is_dir {
                Style::default().fg(Color::Blue)
            } else {
                Style::default()
            };

            Line::from(Span::styled(text, style))
        })
        .collect();

    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Files ")
        .border_style(border_style);

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_file(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.focus, Focus::File);
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };

    if app.file_path.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" No file open ")
            .border_style(border_style);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let hint = Line::from(Span::styled(
            "Open a file from the tree (Enter / l)",
            Style::default().fg(Color::DarkGray),
        ));
        let y = inner.height / 2;
        let hint_area = Rect {
            y: inner.y + y,
            height: 1,
            ..inner
        };
        f.render_widget(Paragraph::new(hint).alignment(Alignment::Center), hint_area);
        return;
    }

    let mode_tag = match app.view_mode {
        ViewMode::Source => "[src]",
        ViewMode::LatexCompiled => "[tex]",
        ViewMode::GitDiff => "[diff]",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} {} ", app.file_path, mode_tag))
        .title_style(Style::default().add_modifier(Modifier::BOLD))
        .border_style(border_style);

    match app.view_mode {
        ViewMode::Source => render_source(f, app, area, block),
        ViewMode::LatexCompiled => render_latex_compiled(f, app, area, block),
        ViewMode::GitDiff => render_git_diff(f, app, area, block),
    }
}

fn render_source(f: &mut Frame, app: &App, area: Rect, block: Block) {
    let text_width = (area.width as usize).saturating_sub(2 + GUTTER).max(1);
    let inner_height = area.height.saturating_sub(2) as usize;
    let mut display: Vec<Line> = Vec::new();
    let mut rows = 0;
    let mut i = app.scroll;

    while i < app.lines.len() && rows < inner_height {
        let comment = app.comments.get(&i);
        let hl = app
            .highlighted_lines
            .get(i)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let is_match = !app.search_query.is_empty() && app.search_matches.contains(&i);

        let new_lines = file_lines(
            i,
            hl,
            i == app.cursor,
            comment.is_some(),
            is_match,
            text_width,
        );
        rows += new_lines.len();
        display.extend(new_lines);

        if let Some(comment) = comment {
            for line in inline_comment_bordered(comment, area.width) {
                if rows < inner_height {
                    display.push(line);
                    rows += 1;
                }
            }
        }

        i += 1;
    }

    f.render_widget(Paragraph::new(display).block(block), area);
}

fn render_latex_compiled(f: &mut Frame, app: &App, area: Rect, block: Block) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let text_width = (area.width as usize).saturating_sub(2 + GUTTER).max(1);
    let mut display: Vec<Line> = Vec::new();
    let mut rows = 0;

    let lines = match &app.compiled_lines {
        Some(l) => l.as_slice(),
        None => &[],
    };

    let mut i = app.scroll;
    while i < lines.len() && rows < inner_height {
        let line_text = &lines[i];
        let char_count = line_text.chars().count().max(1);
        let wrapped_rows = char_count.div_ceil(text_width.max(1));
        let mut first = true;
        for chunk_start in (0..char_count).step_by(text_width.max(1)) {
            let chunk: String = line_text
                .chars()
                .skip(chunk_start)
                .take(text_width.max(1))
                .collect();
            let gutter = if first {
                vec![
                    Span::styled("  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{:4} ", i + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled("  ", Style::default()),
                ]
            } else {
                vec![Span::styled(" ".repeat(GUTTER), Style::default())]
            };
            first = false;
            let mut spans = gutter;
            spans.push(Span::raw(chunk));
            display.push(Line::from(spans));
            rows += 1;
            if rows >= inner_height {
                break;
            }
        }
        if wrapped_rows == 0 {
            // empty line
            let spans = vec![
                Span::styled("  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:4} ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled("  ", Style::default()),
            ];
            display.push(Line::from(spans));
            rows += 1;
        }
        i += 1;
    }

    if lines.is_empty() {
        display.push(Line::from(Span::styled(
            "  Loading...",
            Style::default().fg(Color::DarkGray),
        )));
    }

    f.render_widget(Paragraph::new(display).block(block), area);
}

fn render_git_diff(f: &mut Frame, app: &App, area: Rect, block: Block) {
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width < 4 || inner.height < 2 {
        return;
    }

    // Reserve 1 col for separator, split rest evenly
    let half = (inner.width.saturating_sub(1)) / 2;
    let left_area = Rect {
        x: inner.x,
        y: inner.y,
        width: half,
        height: inner.height,
    };
    let sep_area = Rect {
        x: inner.x + half,
        y: inner.y,
        width: 1,
        height: inner.height,
    };
    let right_area = Rect {
        x: inner.x + half + 1,
        y: inner.y,
        width: inner.width.saturating_sub(half + 1),
        height: inner.height,
    };

    const DIFF_GUTTER: usize = 6;
    const RIGHT_GUTTER: usize = 9; // "▶ "(2) + "1234 "(5) + "● "(2)
    let left_text_w = (half as usize).saturating_sub(DIFF_GUTTER).max(1);
    let right_text_w = (right_area.width as usize).saturating_sub(RIGHT_GUTTER).max(1);

    let rows = match &app.diff_rows {
        Some(r) => r.as_slice(),
        None => &[],
    };

    // Draw separator column
    let sep_lines: Vec<Line> = (0..inner.height)
        .map(|_| Line::from(Span::styled("│", Style::default().fg(Color::DarkGray))))
        .collect();
    f.render_widget(Paragraph::new(sep_lines), sep_area);

    if rows.is_empty() {
        let msg = Line::from(Span::styled(
            "  No changes vs HEAD",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(
            Paragraph::new(vec![msg.clone()]).alignment(Alignment::Left),
            left_area,
        );
        f.render_widget(
            Paragraph::new(vec![msg]).alignment(Alignment::Left),
            right_area,
        );
        return;
    }

    // Headers
    let mut left_display: Vec<Line> = vec![Line::from(Span::styled(
        " HEAD",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    ))];
    let mut right_display: Vec<Line> = vec![Line::from(Span::styled(
        " Working Tree",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    ))];
    let mut visible = 1usize; // header row

    let inner_h = inner.height as usize;

    for (row_idx, row) in rows.iter().enumerate().skip(app.scroll) {
        if visible >= inner_h {
            break;
        }

        let is_cursor = row_idx == app.cursor;
        let src_line: Option<usize> = row.new.as_ref().map(|(n, _)| n - 1);
        let comment_text = src_line.and_then(|l| app.comments.get(&l));
        let has_comment = comment_text.is_some();

        let base_old_bg = match row.kind {
            DiffRowKind::Changed if row.old.is_some() => Color::Red,
            _ => Color::Reset,
        };
        let base_new_bg = match row.kind {
            DiffRowKind::Changed if row.new.is_some() => Color::Green,
            _ => Color::Reset,
        };
        let old_bg = if is_cursor { Color::DarkGray } else { base_old_bg };
        let new_bg = if is_cursor { Color::DarkGray } else { base_new_bg };

        let (old_no, old_text) = row
            .old
            .as_ref()
            .map(|(n, s)| (*n, s.as_str()))
            .unwrap_or((0, ""));
        let left_content: String = old_text.chars().take(left_text_w).collect();
        let left_pad = left_text_w.saturating_sub(left_content.chars().count());
        let left_line = Line::from(vec![
            Span::styled(" ", Style::default().bg(old_bg)),
            if row.old.is_some() {
                Span::styled(
                    format!("{:4} ", old_no),
                    Style::default().fg(Color::DarkGray).bg(old_bg),
                )
            } else {
                Span::styled(format!("{:4} ", ""), Style::default().bg(old_bg))
            },
            Span::styled(left_content + &" ".repeat(left_pad), Style::default().bg(old_bg)),
        ]);

        let (new_no, new_text) = row
            .new
            .as_ref()
            .map(|(n, s)| (*n, s.as_str()))
            .unwrap_or((0, ""));
        let right_content: String = new_text.chars().take(right_text_w).collect();
        let right_pad = right_text_w.saturating_sub(right_content.chars().count());
        let right_line = Line::from(vec![
            Span::styled(
                if is_cursor { "▶ " } else { "  " },
                Style::default().fg(Color::Yellow).bg(new_bg),
            ),
            if row.new.is_some() {
                Span::styled(
                    format!("{:4} ", new_no),
                    Style::default().fg(Color::DarkGray).bg(new_bg),
                )
            } else {
                Span::styled(format!("{:4} ", ""), Style::default().bg(new_bg))
            },
            Span::styled(
                if has_comment { "● " } else { "  " },
                Style::default().fg(Color::Cyan).bg(new_bg),
            ),
            Span::styled(right_content + &" ".repeat(right_pad), Style::default().bg(new_bg)),
        ]);

        left_display.push(left_line);
        right_display.push(right_line);
        visible += 1;

        if let Some(text) = comment_text {
            for box_line in inline_comment_bordered(text, right_area.width) {
                if visible >= inner_h {
                    break;
                }
                left_display.push(Line::default());
                right_display.push(box_line);
                visible += 1;
            }
        }
    }

    f.render_widget(Paragraph::new(left_display), left_area);
    f.render_widget(Paragraph::new(right_display), right_area);
}

fn wrap_spans(
    hl_spans: &[(SyntectColor, String)],
    available_width: usize,
    bg: Color,
) -> Vec<Vec<Span<'static>>> {
    let available_width = available_width.max(1);
    let mut rows: Vec<Vec<Span<'static>>> = vec![vec![]];
    let mut col = 0usize;

    for (fg, text) in hl_spans {
        let fg_color = Color::Rgb(fg.r, fg.g, fg.b);
        let style = Style::default().fg(fg_color).bg(bg);
        let mut chunk = String::new();

        for ch in text.chars() {
            if col == available_width {
                if !chunk.is_empty() {
                    rows.last_mut()
                        .unwrap()
                        .push(Span::styled(chunk.clone(), style));
                    chunk.clear();
                }
                rows.push(vec![]);
                col = 0;
            }
            chunk.push(ch);
            col += 1;
        }

        if !chunk.is_empty() {
            rows.last_mut().unwrap().push(Span::styled(chunk, style));
        }
    }

    rows
}

fn file_lines(
    idx: usize,
    hl_spans: &[(SyntectColor, String)],
    is_cursor: bool,
    has_comment: bool,
    is_match: bool,
    text_width: usize,
) -> Vec<Line<'static>> {
    let bg = if is_cursor {
        Color::DarkGray
    } else {
        Color::Reset
    };
    let wrapped = wrap_spans(hl_spans, text_width, bg);
    let mut lines = Vec::new();

    for (row, spans) in wrapped.into_iter().enumerate() {
        let mut row_spans: Vec<Span<'static>> = if row == 0 {
            vec![
                Span::styled(
                    if is_cursor {
                        "▶ "
                    } else if is_match {
                        "◆ "
                    } else {
                        "  "
                    },
                    Style::default().fg(Color::Yellow).bg(bg),
                ),
                Span::styled(
                    format!("{:4} ", idx + 1),
                    Style::default().fg(Color::DarkGray).bg(bg),
                ),
                Span::styled(
                    if has_comment { "● " } else { "  " },
                    Style::default().fg(Color::Cyan).bg(bg),
                ),
            ]
        } else {
            vec![Span::styled(" ".repeat(GUTTER), Style::default().bg(bg))]
        };

        row_spans.extend(spans);
        lines.push(Line::from(row_spans));
    }

    lines
}

fn inline_comment_bordered(text: &str, area_width: u16) -> Vec<Line<'static>> {
    let pad = "         "; // 9 spaces = GUTTER
    let text_width = comment_text_width(area_width);
    let border_width = text_width + 2;
    let border = Style::default().fg(Color::DarkGray);

    let mut lines = vec![Line::from(Span::styled(
        format!("{}┌{}┐", pad, comment_top_border(border_width)),
        border,
    ))];

    lines.extend(wrap_text(text, text_width).into_iter().map(|line| {
        Line::from(vec![
            Span::styled(format!("{}│ ", pad), border),
            Span::styled(
                format!("{:<width$}", line, width = text_width),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(" │".to_owned(), border),
        ])
    }));

    lines.push(Line::from(Span::styled(
        format!("{}└{}┘", pad, "─".repeat(border_width)),
        border,
    )));

    lines
}

pub(crate) fn line_display_rows(line: &str, view_width: u16) -> usize {
    let available = (view_width as usize).saturating_sub(2 + GUTTER).max(1);
    let n = line.chars().count();
    if n == 0 { 1 } else { n.div_ceil(available) }
}

pub(crate) fn comment_box_height(text: &str, area_width: u16) -> usize {
    wrap_text(text, comment_text_width(area_width)).len() + 2
}

pub(crate) fn comment_editor_height(text: &str, area_width: u16) -> u16 {
    let visible = format!("{text}_");
    let text_width = (area_width as usize).saturating_sub(2).max(1);
    (visible.chars().count().div_ceil(text_width).max(1) + 2) as u16
}

pub(crate) fn file_area_width(total_width: u16, tree_width_pct: u16) -> u16 {
    let tree_w = (total_width as u32 * tree_width_pct as u32 / 100) as u16;
    total_width.saturating_sub(tree_w)
}

fn comment_text_width(area_width: u16) -> usize {
    (area_width as usize).saturating_sub(15).max(1)
}

fn comment_top_border(width: usize) -> String {
    let title = "─ note ";
    let title_width = title.chars().count();
    if width >= title_width {
        format!("{title}{}", "─".repeat(width - title_width))
    } else {
        "─".repeat(width)
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();

    for raw_line in text.lines() {
        let mut line = String::new();

        for word in raw_line.split_whitespace() {
            if line.is_empty() {
                push_wrapped_word(&mut lines, &mut line, word, width);
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(line);
                line = String::new();
                push_wrapped_word(&mut lines, &mut line, word, width);
            }
        }

        lines.push(line);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

fn push_wrapped_word(lines: &mut Vec<String>, line: &mut String, word: &str, width: usize) {
    for ch in word.chars() {
        if line.chars().count() == width {
            lines.push(std::mem::take(line));
        }
        line.push(ch);
    }
}

fn render_comment_edit(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Comment (line {}) ", app.editing_line + 1))
        .border_style(Style::default().fg(Color::Yellow));

    f.render_widget(
        Paragraph::new(format!("{}_", app.input))
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_display_rows_empty_is_one() {
        assert_eq!(line_display_rows("", 80), 1);
    }

    #[test]
    fn line_display_rows_short_fits_in_one() {
        assert_eq!(line_display_rows("hello", 80), 1);
    }

    #[test]
    fn line_display_rows_exactly_fills_width() {
        // available = 80 - 2 - GUTTER(9) = 69
        let line = "a".repeat(69);
        assert_eq!(line_display_rows(&line, 80), 1);
    }

    #[test]
    fn line_display_rows_one_over_wraps() {
        let line = "a".repeat(70);
        assert_eq!(line_display_rows(&line, 80), 2);
    }

    #[test]
    fn comment_box_height_empty_text_is_three() {
        // wrap_text("", w) → [""], 1 line + 2 borders = 3
        assert_eq!(comment_box_height("", 80), 3);
    }

    #[test]
    fn comment_box_height_short_text_is_three() {
        assert_eq!(comment_box_height("hello", 80), 3);
    }

    #[test]
    fn comment_box_height_long_word_wraps() {
        // comment_text_width(80) = 65; a 66-char word forces 2 content rows → height 4
        let text = "a".repeat(66);
        assert_eq!(comment_box_height(&text, 80), 4);
    }

    #[test]
    fn file_area_width_25_pct_of_100() {
        // tree = 25, file = 75
        assert_eq!(file_area_width(100, 25), 75);
    }

    #[test]
    fn file_area_width_50_pct_of_200() {
        assert_eq!(file_area_width(200, 50), 100);
    }

    #[test]
    fn file_area_width_zero_total() {
        assert_eq!(file_area_width(0, 25), 0);
    }

    #[test]
    fn wrap_text_single_short_word() {
        assert_eq!(wrap_text("hello", 20), vec!["hello"]);
    }

    #[test]
    fn wrap_text_wraps_on_word_boundary() {
        // "hello world" width 8: "hello"(5) fits, "world"(5) makes 5+1+5=11 > 8 → new line
        assert_eq!(wrap_text("hello world", 8), vec!["hello", "world"]);
    }

    #[test]
    fn wrap_text_empty_yields_one_empty_line() {
        assert_eq!(wrap_text("", 10), vec![""]);
    }

    #[test]
    fn wrap_text_long_word_no_panic() {
        // word longer than width must not panic and must preserve all chars
        let result = wrap_text("abcdefghij", 5);
        assert!(!result.is_empty());
        assert_eq!(result.join(""), "abcdefghij");
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(msg) = &app.status {
        Line::from(vec![
            Span::raw(" "),
            Span::styled(msg.clone(), Style::default().fg(Color::Green)),
        ])
    } else {
        match (&app.mode, &app.focus) {
            (Mode::EditComment, _) => Line::from(vec![
                Span::styled(
                    " INSERT ",
                    Style::default()
                        .bg(Color::Green)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  Enter:confirm  Esc:cancel"),
            ]),
            (Mode::Search, _) => Line::from(vec![
                Span::styled(
                    " SEARCH ",
                    Style::default()
                        .bg(Color::Magenta)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  /"),
                Span::styled(app.search_input.clone(), Style::default().fg(Color::White)),
                Span::styled("_", Style::default().fg(Color::DarkGray)),
                Span::raw("  Enter:confirm  Esc:cancel"),
            ]),
            (Mode::Normal, Focus::Tree) => Line::from(vec![
                Span::styled(
                    " TREE ",
                    Style::default()
                        .bg(Color::Cyan)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(
                    "  j/k/scroll:nav  Enter/l:open  Space/h:expand  Tab:switch  E:global-export  </>:resize  q:quit",
                ),
            ]),
            (Mode::Normal, Focus::File) => Line::from(vec![
                Span::styled(
                    " NORMAL ",
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(
                    "  j/k/scroll:nav  /:search  n/N:next/prev  c:comment  d:del  D:del-all  y:yank  Y:yank-issues  v:mode  E:global-export  </>:resize  g/G:top/bot  Tab:switch  q:quit",
                ),
            ]),
        }
    };

    f.render_widget(Paragraph::new(line), area);
}
