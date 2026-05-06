use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use syntect::highlighting::Color as SyntectColor;

use crate::app::{App, Focus, Mode};

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

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
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

    let (title, block) = if app.file_path.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" No file open ")
            .border_style(border_style);
        (None, block)
    } else {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", app.file_path))
            .title_style(Style::default().add_modifier(Modifier::BOLD))
            .border_style(border_style);
        (Some(()), block)
    };

    if title.is_none() {
        let inner = block.inner(area);
        f.render_widget(block, area);
        let hint = Line::from(Span::styled(
            "Open a file from the tree (Enter / l)",
            Style::default().fg(Color::DarkGray),
        ));
        let y = inner.height / 2;
        let hint_area = Rect { y: inner.y + y, height: 1, ..inner };
        f.render_widget(Paragraph::new(hint).alignment(Alignment::Center), hint_area);
        return;
    }

    let text_width = (area.width as usize).saturating_sub(2 + GUTTER).max(1);
    let inner_height = area.height.saturating_sub(2) as usize;
    let mut display: Vec<Line> = Vec::new();
    let mut rows = 0;
    let mut i = app.scroll;

    while i < app.lines.len() && rows < inner_height {
        let comment = app.comments.get(&i);
        let hl = app.highlighted_lines.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
        let is_match = !app.search_query.is_empty() && app.search_matches.contains(&i);

        let new_lines = file_lines(i, hl, i == app.cursor, comment.is_some(), is_match, text_width);
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
                    rows.last_mut().unwrap().push(Span::styled(chunk.clone(), style));
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
    let bg = if is_cursor { Color::DarkGray } else { Color::Reset };
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

pub(crate) fn file_area_width(total_width: u16) -> u16 {
    total_width.saturating_sub(total_width / 4)
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
        .title(format!(" Comment (line {}) ", app.cursor + 1))
        .border_style(Style::default().fg(Color::Yellow));

    f.render_widget(
        Paragraph::new(format!("{}_", app.input))
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false }),
        area,
    );
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
                Span::raw("  j/k:nav  Enter/l:open  Space/h:expand  Tab:switch  q:quit"),
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
                    "  j/k:nav  /:search  n/N:next/prev  c:comment  d:del  D:del-all  y:yank  Y:yank-issues  g/G:top/bot  Tab:switch  q:quit",
                ),
            ]),
        }
    };

    f.render_widget(Paragraph::new(line), area);
}
