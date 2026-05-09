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
        let hint_area = Rect { y: inner.y + y, height: 1, ..inner };
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
            let chunk: String = line_text.chars().skip(chunk_start).take(text_width.max(1)).collect();
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
                Span::styled(format!("{:4} ", i + 1), Style::default().fg(Color::DarkGray)),
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
    let left_area = Rect { x: inner.x, y: inner.y, width: half, height: inner.height };
    let sep_area = Rect { x: inner.x + half, y: inner.y, width: 1, height: inner.height };
    let right_area = Rect {
        x: inner.x + half + 1,
        y: inner.y,
        width: inner.width.saturating_sub(half + 1),
        height: inner.height,
    };

    const DIFF_GUTTER: usize = 6; // " 1234 "
    let text_w = (half as usize).saturating_sub(DIFF_GUTTER).max(1);

    let rows = match &app.diff_rows {
        Some(r) => r.as_slice(),
        None => &[],
    };

    // Draw separator column
    let sep_lines: Vec<Line> = (0..inner.height).map(|_| Line::from(Span::styled("│", Style::default().fg(Color::DarkGray)))).collect();
    f.render_widget(Paragraph::new(sep_lines), sep_area);

    if rows.is_empty() {
        let msg = Line::from(Span::styled(
            "  No changes vs HEAD",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(Paragraph::new(vec![msg.clone()]).alignment(Alignment::Left), left_area);
        f.render_widget(Paragraph::new(vec![msg]).alignment(Alignment::Left), right_area);
        return;
    }

    // Headers
    let mut left_display: Vec<Line> = vec![Line::from(Span::styled(
        " HEAD",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    ))];
    let mut right_display: Vec<Line> = vec![Line::from(Span::styled(
        " Working Tree",
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
    ))];
    let mut visible = 1usize; // header row

    let inner_h = inner.height as usize;

    for row in rows.iter().skip(app.scroll) {
        if visible >= inner_h {
            break;
        }

        let (old_bg, new_bg) = match row.kind {
            DiffRowKind::Context => (Color::Reset, Color::Reset),
            DiffRowKind::Changed => (
                if row.old.is_some() { Color::Red } else { Color::Reset },
                if row.new.is_some() { Color::Green } else { Color::Reset },
            ),
        };

        let old_content = row.old.as_ref().map(|(n, s)| (*n, s.as_str())).unwrap_or((0, ""));
        let new_content = row.new.as_ref().map(|(n, s)| (*n, s.as_str())).unwrap_or((0, ""));

        let fmt_side = |line_no: usize, text: &str, bg: Color, present: bool| -> Line<'static> {
            let num_span = if present {
                Span::styled(
                    format!("{:4} ", line_no),
                    Style::default().fg(Color::DarkGray).bg(bg),
                )
            } else {
                Span::styled(format!("{:4} ", ""), Style::default().bg(bg))
            };
            let content: String = text.chars().take(text_w).collect();
            let pad = text_w.saturating_sub(content.chars().count());
            Line::from(vec![
                Span::styled(" ", Style::default().bg(bg)),
                num_span,
                Span::styled(content + &" ".repeat(pad), Style::default().bg(bg)),
            ])
        };

        left_display.push(fmt_side(old_content.0, old_content.1, old_bg, row.old.is_some()));
        right_display.push(fmt_side(new_content.0, new_content.1, new_bg, row.new.is_some()));
        visible += 1;
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
                Span::raw("  j/k/scroll:nav  Enter/l:open  Space/h:expand  Tab:switch  E:global-export  </>:resize  q:quit"),
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
