use std::{collections::HashMap, fs, io, path::Path, sync::mpsc::Receiver, time::Duration};

use syntect::{
    easy::HighlightLines,
    highlighting::{Color as SyntectColor, Theme, ThemeSet},
    parsing::SyntaxSet,
};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::{Terminal, backend::CrosstermBackend};
use rusqlite::{Connection, params};

use crate::{tree::FileTree, ui};

fn open_db() -> Result<Connection> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("could not find config directory"))?
        .join("tuitr");
    fs::create_dir_all(&config_dir)?;
    let conn = Connection::open(config_dir.join("comments.db"))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS comments (
            file_path   TEXT    NOT NULL,
            line_number INTEGER NOT NULL,
            comment     TEXT    NOT NULL,
            PRIMARY KEY (file_path, line_number)
        );",
    )?;
    Ok(conn)
}

fn load_file_comments(db: &Connection, file_path: &str) -> HashMap<usize, String> {
    db.prepare("SELECT line_number, comment FROM comments WHERE file_path = ?1")
        .and_then(|mut stmt| {
            stmt.query_map([file_path], |row| {
                Ok((row.get::<_, i64>(0)? as usize, row.get::<_, String>(1)?))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
}

fn set_clipboard(text: String) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let cmds: &[(&str, &[&str])] = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[("wl-copy", &[] as &[&str])]
    } else {
        &[
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };

    for (cmd, args) in cmds {
        let Ok(mut child) = Command::new(cmd).args(*args).stdin(Stdio::piped()).spawn() else {
            continue;
        };
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        child.wait()?;
        return Ok(());
    }

    arboard::Clipboard::new()?.set_text(text)?;
    Ok(())
}

fn check_file_displayable(path: &str) -> Result<(), String> {
    use std::io::Read;
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.len() > 10 * 1024 * 1024 {
        return Err("File too large to display (> 10 MB)".to_string());
    }
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).map_err(|e| e.to_string())?;
    if buf[..n].contains(&0u8) {
        return Err("Binary file, cannot display".to_string());
    }
    Ok(())
}

pub enum Mode {
    Normal,
    EditComment,
    Search,
}

pub enum Focus {
    Tree,
    File,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ViewMode {
    Source,
    LatexCompiled,
    GitDiff,
}

#[derive(Clone)]
pub enum DiffRowKind {
    Context,
    Changed,
}

#[derive(Clone)]
pub struct DiffRow {
    pub old: Option<(usize, String)>,
    pub new: Option<(usize, String)>,
    pub kind: DiffRowKind,
}

pub struct App {
    pub file_path: String,
    pub lines: Vec<String>,
    pub comments: HashMap<usize, String>,
    pub cursor: usize,
    pub editing_line: usize,
    pub scroll: usize,
    pub view_height: usize,
    pub view_width: u16,
    pub tree_width_pct: u16,
    pub mode: Mode,
    pub focus: Focus,
    pub input: String,
    pub status: Option<String>,
    pub tree: FileTree,
    pub highlighted_lines: Vec<Vec<(SyntectColor, String)>>,
    pub search_query: String,
    pub search_input: String,
    pub search_matches: Vec<usize>,
    search_match_idx: usize,
    pub view_mode: ViewMode,
    pub compiled_lines: Option<Vec<String>>,
    pub diff_rows: Option<Vec<DiffRow>>,
    syntax_set: SyntaxSet,
    theme: Theme,
    db: Connection,
    watcher: RecommendedWatcher,
    watch_rx: Receiver<notify::Result<notify::Event>>,
}

impl App {
    pub fn new(path: String) -> Result<Self> {
        let abs = fs::canonicalize(&path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.clone());

        let is_dir = Path::new(&abs).is_dir();

        let mut initial_status: Option<String> = None;

        let (root, file_path, lines) = if is_dir {
            (Path::new(&abs).to_path_buf(), String::new(), Vec::new())
        } else {
            let root = Path::new(&abs)
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();
            match check_file_displayable(&abs) {
                Ok(()) => {
                    let content = fs::read_to_string(&abs)?;
                    let lines = content.lines().map(String::from).collect();
                    (root, abs, lines)
                }
                Err(msg) => {
                    initial_status = Some(msg);
                    (root, String::new(), Vec::new())
                }
            }
        };

        let db = open_db()?;
        let comments = if file_path.is_empty() {
            HashMap::new()
        } else {
            load_file_comments(&db, &file_path)
        };

        let syntax_set = SyntaxSet::load_defaults_nonewlines();
        let theme = ThemeSet::load_defaults().themes["base16-ocean.dark"].clone();

        let (tx, watch_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
        let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())?;
        if !file_path.is_empty() {
            let _ = watcher.watch(Path::new(&file_path), RecursiveMode::NonRecursive);
        }

        let mut app = Self {
            file_path,
            lines,
            comments,
            cursor: 0,
            editing_line: 0,
            scroll: 0,
            view_height: 20,
            view_width: 80,
            tree_width_pct: 25,
            mode: Mode::Normal,
            focus: if is_dir { Focus::Tree } else { Focus::File },
            input: String::new(),
            status: None,
            tree: FileTree::new(root),
            highlighted_lines: Vec::new(),
            search_query: String::new(),
            search_input: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
            view_mode: ViewMode::Source,
            compiled_lines: None,
            diff_rows: None,
            syntax_set,
            theme,
            db,
            watcher,
            watch_rx,
        };
        app.rehighlight();
        app.status = initial_status;
        Ok(app)
    }

    pub fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        loop {
            while let Ok(Ok(event)) = self.watch_rx.try_recv() {
                if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_))
                    && let Err(e) = self.reload_file()
                {
                    self.status = Some(format!("Reload error: {e}"));
                }
            }

            terminal.draw(|f| {
                let area = f.area();
                let edit_h = if matches!(self.mode, Mode::EditComment) {
                    ui::comment_editor_height(&self.input, area.width)
                } else {
                    0
                };
                self.view_height = area.height.saturating_sub(edit_h + 3) as usize;
                self.view_width = ui::file_area_width(area.width, self.tree_width_pct);
                self.tree.scroll_to_cursor(self.view_height);
                ui::render(f, self);
            })?;

            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) => {
                        if self.handle_key(key) {
                            break;
                        }
                    }
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    _ => {}
                }
            }
        }

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        Ok(())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.status = None;

        if matches!(self.mode, Mode::EditComment) {
            self.handle_edit(key);
            return false;
        }

        if matches!(self.mode, Mode::Search) {
            self.handle_search_input(key);
            return false;
        }

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::Char('<') => {
                self.tree_width_pct = self.tree_width_pct.saturating_sub(5).max(10)
            }
            KeyCode::Char('>') => self.tree_width_pct = (self.tree_width_pct + 5).min(50),
            KeyCode::Char('E') => self.export_all_annotations(),
            _ => match self.focus {
                Focus::Tree => self.handle_tree(key),
                Focus::File => self.handle_file(key),
            },
        }
        false
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollDown => match self.focus {
                Focus::File => self.move_down(),
                Focus::Tree => {
                    self.tree.move_down();
                    self.tree.scroll_to_cursor(self.view_height);
                }
            },
            MouseEventKind::ScrollUp => match self.focus {
                Focus::File => self.move_up(),
                Focus::Tree => {
                    self.tree.move_up();
                    self.tree.scroll_to_cursor(self.view_height);
                }
            },
            _ => {}
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::File,
            Focus::File => Focus::Tree,
        };
    }

    fn handle_tree(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.tree.move_down();
                self.tree.scroll_to_cursor(self.view_height);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.tree.move_up();
                self.tree.scroll_to_cursor(self.view_height);
            }
            KeyCode::Enter | KeyCode::Char('l') => {
                if let Some(path) = self.tree.current_file() {
                    let path: String = path.to_string_lossy().into_owned();
                    match self.load_file(path) {
                        Ok(()) => self.focus = Focus::File,
                        Err(e) => self.status = Some(format!("Error: {e}")),
                    }
                } else {
                    self.tree.toggle_expand();
                }
            }
            KeyCode::Char(' ') | KeyCode::Char('h') => self.tree.toggle_expand(),
            _ => {}
        }
    }

    fn handle_file(&mut self, key: KeyEvent) {
        if self.file_path.is_empty() {
            return;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Char('g') => self.move_top(),
            KeyCode::Char('G') => self.move_bottom(),
            KeyCode::Char('c') => self.start_comment(),
            KeyCode::Char('d') => self.delete_comment(),
            KeyCode::Char('D') => self.delete_all_comments(),
            KeyCode::Char('y') => self.yank(),
            KeyCode::Char('Y') => self.yank_all_comments(),
            KeyCode::Char('/') => self.start_search(),
            KeyCode::Char('n') => self.next_match(),
            KeyCode::Char('N') => self.prev_match(),
            KeyCode::Char('v') => self.switch_view_mode(),
            _ => {}
        }
    }

    fn handle_edit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.confirm_comment(),
            KeyCode::Esc => self.cancel_comment(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    fn handle_search_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.confirm_search(),
            KeyCode::Esc => self.cancel_search(),
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char(c) => self.search_input.push(c),
            _ => {}
        }
    }

    fn view_len(&self) -> usize {
        match self.view_mode {
            ViewMode::Source => self.lines.len(),
            ViewMode::LatexCompiled => {
                self.compiled_lines.as_deref().map(|l| l.len()).unwrap_or(0)
            }
            ViewMode::GitDiff => self.diff_rows.as_deref().map(|r| r.len()).unwrap_or(0),
        }
    }

    pub(crate) fn source_line_for_cursor(&self) -> Option<usize> {
        match self.view_mode {
            ViewMode::Source => Some(self.cursor),
            ViewMode::GitDiff => self
                .diff_rows
                .as_deref()
                .and_then(|rows| rows.get(self.cursor))
                .and_then(|row| row.new.as_ref())
                .map(|(n, _)| n - 1),
            ViewMode::LatexCompiled => None,
        }
    }

    fn move_down(&mut self) {
        if self.cursor + 1 < self.view_len() {
            self.cursor += 1;
            self.scroll_to_cursor();
        }
    }

    fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.scroll_to_cursor();
        }
    }

    fn move_top(&mut self) {
        self.cursor = 0;
        self.scroll = 0;
    }

    fn move_bottom(&mut self) {
        self.cursor = self.view_len().saturating_sub(1);
        self.scroll_to_cursor();
    }

    fn scroll_to_cursor(&mut self) {
        if self.view_height == 0 {
            return;
        }
        match self.view_mode {
            ViewMode::Source => {
                if self.cursor < self.scroll {
                    self.scroll = self.cursor;
                    return;
                }
                loop {
                    let rows: usize = (self.scroll..=self.cursor)
                        .map(|i| {
                            let line = self.lines.get(i).map(|s| s.as_str()).unwrap_or("");
                            ui::line_display_rows(line, self.view_width)
                                + self
                                    .comments
                                    .get(&i)
                                    .map(|c| ui::comment_box_height(c, self.view_width))
                                    .unwrap_or(0)
                        })
                        .sum();
                    if rows <= self.view_height {
                        break;
                    }
                    self.scroll += 1;
                }
            }
            ViewMode::GitDiff => {
                if self.cursor < self.scroll {
                    self.scroll = self.cursor;
                    return;
                }
                let rows = self.diff_rows.as_deref().unwrap_or(&[]);
                loop {
                    let visible: usize = (self.scroll..=self.cursor)
                        .map(|i| {
                            let comment_h = rows
                                .get(i)
                                .and_then(|row| row.new.as_ref().map(|(n, _)| n - 1))
                                .and_then(|src| self.comments.get(&src))
                                .map(|c| ui::comment_box_height(c, self.view_width / 2))
                                .unwrap_or(0);
                            1 + comment_h
                        })
                        .sum();
                    if visible <= self.view_height {
                        break;
                    }
                    self.scroll += 1;
                }
            }
            ViewMode::LatexCompiled => {
                if self.cursor < self.scroll {
                    self.scroll = self.cursor;
                } else if self.cursor >= self.scroll + self.view_height {
                    self.scroll = self.cursor.saturating_sub(self.view_height - 1);
                }
            }
        }
    }

    pub(crate) fn start_comment(&mut self) {
        match self.source_line_for_cursor() {
            None => {
                self.status = Some("Comments not available in compiled view".to_string());
            }
            Some(src_line) => {
                self.editing_line = src_line;
                self.input = self.comments.get(&src_line).cloned().unwrap_or_default();
                self.mode = Mode::EditComment;
            }
        }
    }

    fn confirm_comment(&mut self) {
        let line = self.editing_line;
        let text = self.input.trim().to_string();
        if text.is_empty() {
            self.comments.remove(&line);
            let _ = self.db.execute(
                "DELETE FROM comments WHERE file_path = ?1 AND line_number = ?2",
                params![self.file_path, line as i64],
            );
        } else {
            self.comments.insert(line, text.clone());
            let _ = self.db.execute(
                "INSERT OR REPLACE INTO comments (file_path, line_number, comment) VALUES (?1, ?2, ?3)",
                params![self.file_path, line as i64, text],
            );
        }
        self.input.clear();
        self.mode = Mode::Normal;
    }

    fn cancel_comment(&mut self) {
        self.input.clear();
        self.mode = Mode::Normal;
    }

    pub(crate) fn delete_comment(&mut self) {
        match self.source_line_for_cursor() {
            None => {
                self.status = Some("Comments not available in compiled view".to_string());
            }
            Some(src_line) => {
                if self.comments.remove(&src_line).is_some() {
                    let _ = self.db.execute(
                        "DELETE FROM comments WHERE file_path = ?1 AND line_number = ?2",
                        params![self.file_path, src_line as i64],
                    );
                    self.status = Some("Comment deleted".to_string());
                }
            }
        }
    }

    fn delete_all_comments(&mut self) {
        let count = self.comments.len();
        if count == 0 {
            self.status = Some("No comments to delete".to_string());
            return;
        }
        self.comments.clear();
        let _ = self.db.execute(
            "DELETE FROM comments WHERE file_path = ?1",
            params![self.file_path],
        );
        self.status = Some(format!("Deleted {count} comment(s)"));
    }

    fn yank(&mut self) {
        let Some(line) = self.lines.get(self.cursor) else {
            return;
        };
        let text = if let Some(comment) = self.comments.get(&self.cursor) {
            format!("Line {}: {}\nComment: {}", self.cursor + 1, line, comment)
        } else {
            format!("Line {}: {}", self.cursor + 1, line)
        };
        match set_clipboard(text) {
            Ok(_) => self.status = Some("Yanked to clipboard".to_string()),
            Err(e) => self.status = Some(format!("Clipboard error: {e}")),
        }
    }

    fn yank_all_comments(&mut self) {
        if self.comments.is_empty() {
            self.status = Some("No comments to yank".to_string());
            return;
        }

        let mut entries: Vec<usize> = self.comments.keys().copied().collect();
        entries.sort_unstable();

        let mut text = format!(
            "I found the following issues in {}. Please create a plan to fix them:\n",
            self.file_path
        );

        for line_idx in entries {
            let code = self.lines.get(line_idx).map(|s| s.as_str()).unwrap_or("");
            let comment = &self.comments[&line_idx];
            text.push_str(&format!(
                "\nLine {} (`{}`):\n{}\n",
                line_idx + 1,
                code,
                comment
            ));
        }

        let count = self.comments.len();
        match set_clipboard(text) {
            Ok(_) => self.status = Some(format!("Yanked {count} comment(s) to clipboard")),
            Err(e) => self.status = Some(format!("Clipboard error: {e}")),
        }
    }

    fn export_all_annotations(&mut self) {
        let rows: Vec<(String, i64, String)> = match self
            .db
            .prepare(
                "SELECT file_path, line_number, comment FROM comments ORDER BY file_path, line_number",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
            }) {
            Ok(r) => r,
            Err(e) => {
                self.status = Some(format!("DB error: {e}"));
                return;
            }
        };

        if rows.is_empty() {
            self.status = Some("No annotations in database".to_string());
            return;
        }

        let mut text = "I found the following issues across multiple files. Please create a plan to fix them:\n".to_string();
        let mut current_file = String::new();
        let mut file_lines: Vec<String> = Vec::new();

        for (file_path, line_number, comment) in &rows {
            if *file_path != current_file {
                current_file = file_path.clone();
                file_lines = fs::read_to_string(file_path)
                    .map(|c| c.lines().map(String::from).collect())
                    .unwrap_or_default();
                text.push_str(&format!("\n### {file_path}\n"));
            }
            let code = file_lines
                .get(*line_number as usize)
                .map(|s| s.as_str())
                .unwrap_or("");
            text.push_str(&format!(
                "\nLine {} (`{}`):\n{}\n",
                line_number + 1,
                code,
                comment
            ));
        }

        let count = rows.len();
        match set_clipboard(text) {
            Ok(_) => self.status = Some(format!("Exported {count} annotation(s) globally")),
            Err(e) => self.status = Some(format!("Clipboard error: {e}")),
        }
    }

    fn start_search(&mut self) {
        self.search_input = self.search_query.clone();
        self.mode = Mode::Search;
    }

    fn confirm_search(&mut self) {
        self.search_query = self.search_input.trim().to_string();
        self.mode = Mode::Normal;
        self.compute_matches();
        if !self.search_matches.is_empty() {
            let next = self
                .search_matches
                .iter()
                .find(|&&i| i >= self.cursor)
                .copied()
                .unwrap_or(self.search_matches[0]);
            self.cursor = next;
            self.search_match_idx = self
                .search_matches
                .iter()
                .position(|&i| i == self.cursor)
                .unwrap_or(0);
            self.scroll_to_cursor();
        } else if !self.search_query.is_empty() {
            self.status = Some(format!("No matches for \"{}\"", self.search_query));
        }
    }

    fn cancel_search(&mut self) {
        self.search_input.clear();
        self.mode = Mode::Normal;
    }

    fn compute_matches(&mut self) {
        if self.search_query.is_empty() {
            self.search_matches.clear();
            return;
        }
        let query = self.search_query.to_lowercase();
        self.search_matches = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect();
        self.search_match_idx = 0;
    }

    fn next_match(&mut self) {
        if self.search_matches.is_empty() {
            if !self.search_query.is_empty() {
                self.status = Some("No matches".to_string());
            }
            return;
        }
        self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
        self.cursor = self.search_matches[self.search_match_idx];
        self.scroll_to_cursor();
    }

    fn prev_match(&mut self) {
        if self.search_matches.is_empty() {
            if !self.search_query.is_empty() {
                self.status = Some("No matches".to_string());
            }
            return;
        }
        self.search_match_idx = self
            .search_match_idx
            .checked_sub(1)
            .unwrap_or(self.search_matches.len() - 1);
        self.cursor = self.search_matches[self.search_match_idx];
        self.scroll_to_cursor();
    }

    fn available_view_modes(&self) -> Vec<ViewMode> {
        let mut modes = vec![ViewMode::Source];
        let ext = Path::new(&self.file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if ext.eq_ignore_ascii_case("tex") {
            modes.push(ViewMode::LatexCompiled);
        }
        let in_git = std::process::Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(
                Path::new(&self.file_path)
                    .parent()
                    .unwrap_or(Path::new(".")),
            )
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if in_git {
            modes.push(ViewMode::GitDiff);
        }
        modes
    }

    fn switch_view_mode(&mut self) {
        if self.file_path.is_empty() {
            return;
        }
        let modes = self.available_view_modes();
        let cur = modes.iter().position(|m| *m == self.view_mode).unwrap_or(0);
        let next = modes[(cur + 1) % modes.len()];
        self.view_mode = next;
        self.scroll = 0;
        match next {
            ViewMode::LatexCompiled => self.compile_latex(),
            ViewMode::GitDiff => self.compute_diff(),
            ViewMode::Source => {}
        }
    }

    fn compile_latex(&mut self) {
        let output = std::process::Command::new("pandoc")
            .args(["--from=latex", "--to=plain", &self.file_path])
            .output();
        self.compiled_lines = Some(match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_owned)
                .collect(),
            Ok(o) => {
                let msg = String::from_utf8_lossy(&o.stderr).trim().to_owned();
                vec![format!("[pandoc error: {msg}]")]
            }
            Err(e) => vec![format!("[pandoc not found: {e}]")],
        });
    }

    fn compute_diff(&mut self) {
        use similar::{ChangeTag, TextDiff};

        let file_path = self.file_path.clone();

        // Find git root
        let git_root = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(Path::new(&file_path).parent().unwrap_or(Path::new(".")))
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_owned())
                } else {
                    None
                }
            });

        let Some(root) = git_root else {
            self.diff_rows = Some(vec![DiffRow {
                old: Some((0, "[not a git repository]".into())),
                new: None,
                kind: DiffRowKind::Changed,
            }]);
            return;
        };

        // Get relative path for git show
        let rel = Path::new(&file_path)
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file_path.clone());

        let head_output = std::process::Command::new("git")
            .args(["show", &format!("HEAD:{rel}")])
            .current_dir(&root)
            .output();

        let head_text = match head_output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => String::new(),
        };

        let current_text = self.lines.join("\n") + "\n";

        if head_text == current_text || (head_text.is_empty() && self.lines.is_empty()) {
            self.diff_rows = Some(vec![]);
            return;
        }

        let diff = TextDiff::from_lines(&head_text, &current_text);
        let mut rows: Vec<DiffRow> = Vec::new();

        for group in diff.grouped_ops(3) {
            for op in &group {
                let old_range = op.old_range();
                let new_range = op.new_range();
                let head_lines: Vec<String> = head_text.lines().collect::<Vec<_>>()[old_range
                    .start
                    .min(head_text.lines().count())
                    ..old_range.end.min(head_text.lines().count())]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                let new_lines: Vec<String> = current_text.lines().collect::<Vec<_>>()[new_range
                    .start
                    .min(current_text.lines().count())
                    ..new_range.end.min(current_text.lines().count())]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();

                // Use the change iterator for this op to get accurate tags
                let changes: Vec<_> = diff.iter_changes(op).collect();
                let mut deletes: Vec<(usize, String)> = Vec::new();
                let mut inserts: Vec<(usize, String)> = Vec::new();
                let mut contexts: Vec<(usize, usize, String)> = Vec::new();

                let _ = (head_lines, new_lines); // suppress unused warning

                for change in &changes {
                    match change.tag() {
                        ChangeTag::Equal => {
                            let old_idx = change.old_index().unwrap_or(0);
                            let new_idx = change.new_index().unwrap_or(0);
                            contexts.push((
                                old_idx,
                                new_idx,
                                change.value().trim_end_matches('\n').to_owned(),
                            ));
                        }
                        ChangeTag::Delete => {
                            let idx = change.old_index().unwrap_or(0);
                            deletes.push((idx, change.value().trim_end_matches('\n').to_owned()));
                        }
                        ChangeTag::Insert => {
                            let idx = change.new_index().unwrap_or(0);
                            inserts.push((idx, change.value().trim_end_matches('\n').to_owned()));
                        }
                    }
                }

                // Context lines first (they appear before changes in grouped_ops equal sections)
                for (old_idx, new_idx, text) in contexts {
                    rows.push(DiffRow {
                        old: Some((old_idx + 1, text.clone())),
                        new: Some((new_idx + 1, text)),
                        kind: DiffRowKind::Context,
                    });
                }

                // Pair up deletes and inserts
                let max = deletes.len().max(inserts.len());
                for i in 0..max {
                    rows.push(DiffRow {
                        old: deletes.get(i).cloned().map(|(n, s)| (n + 1, s)),
                        new: inserts.get(i).cloned().map(|(n, s)| (n + 1, s)),
                        kind: DiffRowKind::Changed,
                    });
                }
            }
        }

        self.diff_rows = Some(rows);
    }

    fn load_file(&mut self, path: String) -> Result<()> {
        if let Err(msg) = check_file_displayable(&path) {
            self.status = Some(msg);
            return Ok(());
        }

        if !self.file_path.is_empty() {
            let _ = self.watcher.unwatch(Path::new(&self.file_path));
        }
        let _ = self
            .watcher
            .watch(Path::new(&path), RecursiveMode::NonRecursive);

        let content = fs::read_to_string(&path)?;
        self.lines = content.lines().map(String::from).collect();
        self.comments = load_file_comments(&self.db, &path);
        self.file_path = path;
        self.cursor = 0;
        self.scroll = 0;
        self.view_mode = ViewMode::Source;
        self.compiled_lines = None;
        self.diff_rows = None;
        self.rehighlight();
        self.compute_matches();
        Ok(())
    }

    fn reload_file(&mut self) -> Result<()> {
        if self.file_path.is_empty() {
            return Ok(());
        }
        let content = match fs::read_to_string(&self.file_path) {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        self.lines = content.lines().map(String::from).collect();
        self.cursor = self.cursor.min(self.lines.len().saturating_sub(1));
        self.scroll_to_cursor();
        self.rehighlight();
        self.compute_matches();
        self.status = Some("File reloaded".to_string());
        Ok(())
    }

    #[cfg(test)]
    fn new_for_test(lines: Vec<String>) -> Self {
        use std::sync::mpsc;
        let (tx, watch_rx) = mpsc::channel();
        let watcher = notify::RecommendedWatcher::new(tx, notify::Config::default()).unwrap();
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS comments (
                file_path   TEXT    NOT NULL,
                line_number INTEGER NOT NULL,
                comment     TEXT    NOT NULL,
                PRIMARY KEY (file_path, line_number)
            );",
        )
        .unwrap();
        let syntax_set = SyntaxSet::load_defaults_nonewlines();
        let theme = ThemeSet::load_defaults().themes["base16-ocean.dark"].clone();
        let n = lines.len();
        App {
            file_path: "/test/file.rs".to_string(),
            lines,
            comments: HashMap::new(),
            cursor: 0,
            editing_line: 0,
            scroll: 0,
            view_height: 20,
            view_width: 80,
            tree_width_pct: 25,
            mode: Mode::Normal,
            focus: Focus::File,
            input: String::new(),
            status: None,
            tree: FileTree::new(std::env::temp_dir()),
            highlighted_lines: vec![vec![]; n],
            search_query: String::new(),
            search_input: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
            view_mode: ViewMode::Source,
            compiled_lines: None,
            diff_rows: None,
            syntax_set,
            theme,
            db,
            watcher,
            watch_rx,
        }
    }

    fn rehighlight(&mut self) {
        if self.file_path.is_empty() {
            self.highlighted_lines = Vec::new();
            return;
        }
        let highlighted = {
            let ext = Path::new(&self.file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let syntax = self
                .syntax_set
                .find_syntax_by_extension(ext)
                .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
            let mut h = HighlightLines::new(syntax, &self.theme);
            self.lines
                .iter()
                .map(|line| {
                    h.highlight_line(line, &self.syntax_set)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(style, text)| (style.foreground, text.to_owned()))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        self.highlighted_lines = highlighted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // --- load_file_comments ---

    #[test]
    fn load_file_comments_retrieves_from_db() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS comments (
                file_path TEXT NOT NULL, line_number INTEGER NOT NULL,
                comment TEXT NOT NULL, PRIMARY KEY (file_path, line_number)
            );",
        )
        .unwrap();
        db.execute(
            "INSERT INTO comments VALUES (?1, ?2, ?3)",
            params!["/foo.rs", 3i64, "test note"],
        )
        .unwrap();
        let result = load_file_comments(&db, "/foo.rs");
        assert_eq!(result.get(&3), Some(&"test note".to_string()));
        assert_eq!(result.len(), 1);
    }

    // --- compute_matches ---

    #[test]
    fn compute_matches_case_insensitive() {
        let mut app = App::new_for_test(lines(&["foo bar", "baz", "FOO"]));
        app.search_query = "foo".to_string();
        app.compute_matches();
        assert_eq!(app.search_matches, vec![0, 2]);
    }

    #[test]
    fn compute_matches_empty_query_clears() {
        let mut app = App::new_for_test(lines(&["foo", "bar"]));
        app.search_matches = vec![0];
        app.search_query = String::new();
        app.compute_matches();
        assert!(app.search_matches.is_empty());
    }

    // --- next_match / prev_match ---

    #[test]
    fn next_match_wraps_to_first() {
        let mut app = App::new_for_test(lines(&["a", "b", "a"]));
        app.search_matches = vec![0, 2];
        app.search_match_idx = 1;
        app.cursor = 2;
        app.next_match();
        assert_eq!(app.search_match_idx, 0);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn prev_match_wraps_to_last() {
        let mut app = App::new_for_test(lines(&["a", "b", "a"]));
        app.search_matches = vec![0, 2];
        app.search_match_idx = 0;
        app.cursor = 0;
        app.prev_match();
        assert_eq!(app.search_match_idx, 1);
        assert_eq!(app.cursor, 2);
    }

    // --- navigation bounds ---

    #[test]
    fn move_down_bounded_at_last_line() {
        let mut app = App::new_for_test(lines(&["only"]));
        app.move_down();
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn move_up_bounded_at_zero() {
        let mut app = App::new_for_test(lines(&["a", "b"]));
        app.move_up();
        assert_eq!(app.cursor, 0);
    }

    // --- scroll_to_cursor ---

    #[test]
    fn scroll_advances_when_cursor_below_viewport() {
        let many: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let mut app = App::new_for_test(many);
        app.view_height = 10;
        app.view_width = 80;
        app.cursor = 20;
        app.scroll = 0;
        app.scroll_to_cursor();
        assert!(app.scroll > 0);
        assert!(app.cursor >= app.scroll);
    }

    #[test]
    fn scroll_resets_when_cursor_above_scroll() {
        let many: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let mut app = App::new_for_test(many);
        app.view_height = 10;
        app.view_width = 80;
        app.scroll = 10;
        app.cursor = 5;
        app.scroll_to_cursor();
        assert_eq!(app.scroll, 5);
    }

    // --- comment CRUD ---

    #[test]
    fn confirm_comment_inserts_into_hashmap_and_db() {
        let mut app = App::new_for_test(lines(&["line one", "line two"]));
        app.cursor = 0;
        app.editing_line = 0;
        app.input = "my note".to_string();
        app.confirm_comment();
        assert_eq!(app.comments.get(&0), Some(&"my note".to_string()));
        let fp = app.file_path.clone();
        let count: i64 = app
            .db
            .query_row(
                "SELECT COUNT(*) FROM comments WHERE file_path = ?1 AND line_number = 0",
                params![fp],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn confirm_comment_whitespace_only_removes() {
        let mut app = App::new_for_test(lines(&["line one"]));
        app.cursor = 0;
        app.editing_line = 0;
        app.comments.insert(0, "existing".to_string());
        app.input = "   ".to_string();
        app.confirm_comment();
        assert!(!app.comments.contains_key(&0));
    }

    #[test]
    fn delete_comment_removes_from_hashmap_and_db() {
        let mut app = App::new_for_test(lines(&["line one"]));
        app.cursor = 0;
        app.editing_line = 0;
        app.comments.insert(0, "note".to_string());
        let fp = app.file_path.clone();
        app.db
            .execute(
                "INSERT OR REPLACE INTO comments (file_path, line_number, comment) VALUES (?1, ?2, ?3)",
                params![fp, 0i64, "note"],
            )
            .unwrap();
        app.delete_comment();
        assert!(!app.comments.contains_key(&0));
        let count: i64 = app
            .db
            .query_row(
                "SELECT COUNT(*) FROM comments WHERE file_path = ?1 AND line_number = 0",
                params![fp],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    // --- source_line_for_cursor ---

    #[test]
    fn source_line_for_cursor_source_returns_cursor() {
        let mut app = App::new_for_test(lines(&["a", "b"]));
        app.cursor = 1;
        assert_eq!(app.source_line_for_cursor(), Some(1));
    }

    #[test]
    fn source_line_for_cursor_gitdiff_maps_new_line() {
        let mut app = App::new_for_test(lines(&["a", "b", "c"]));
        app.view_mode = ViewMode::GitDiff;
        app.diff_rows = Some(vec![DiffRow {
            old: None,
            new: Some((3, "c".into())),
            kind: DiffRowKind::Changed,
        }]);
        app.cursor = 0;
        assert_eq!(app.source_line_for_cursor(), Some(2)); // 3 - 1 = 2
    }

    #[test]
    fn source_line_for_cursor_gitdiff_old_only_is_none() {
        let mut app = App::new_for_test(lines(&["a"]));
        app.view_mode = ViewMode::GitDiff;
        app.diff_rows = Some(vec![DiffRow {
            old: Some((1, "a".into())),
            new: None,
            kind: DiffRowKind::Changed,
        }]);
        app.cursor = 0;
        assert_eq!(app.source_line_for_cursor(), None);
    }

    #[test]
    fn source_line_for_cursor_latex_is_none() {
        let mut app = App::new_for_test(lines(&["x"]));
        app.view_mode = ViewMode::LatexCompiled;
        app.compiled_lines = Some(vec!["compiled".into()]);
        assert_eq!(app.source_line_for_cursor(), None);
    }

    // --- view_len / move_down in non-source views ---

    #[test]
    fn move_down_bounded_by_diff_rows() {
        let mut app = App::new_for_test(lines(&["a", "b", "c"]));
        app.view_mode = ViewMode::GitDiff;
        app.diff_rows = Some(vec![DiffRow {
            old: None,
            new: Some((1, "a".into())),
            kind: DiffRowKind::Context,
        }]);
        app.cursor = 0;
        app.move_down();
        assert_eq!(app.cursor, 0); // only 1 diff row, cannot advance
    }

    // --- start_comment in LaTeX view ---

    #[test]
    fn start_comment_latex_blocks_with_status() {
        let mut app = App::new_for_test(lines(&["x"]));
        app.view_mode = ViewMode::LatexCompiled;
        app.compiled_lines = Some(vec!["compiled".into()]);
        app.start_comment();
        assert!(app.status.is_some());
        assert!(matches!(app.mode, Mode::Normal));
    }

    // --- delete_comment in LaTeX view ---

    #[test]
    fn delete_comment_latex_blocks_with_status() {
        let mut app = App::new_for_test(lines(&["x"]));
        app.view_mode = ViewMode::LatexCompiled;
        app.compiled_lines = Some(vec!["compiled".into()]);
        app.delete_comment();
        assert!(app.status.is_some());
    }

    // --- confirm_comment uses editing_line not cursor ---

    #[test]
    fn confirm_comment_uses_editing_line_not_cursor() {
        let mut app = App::new_for_test(lines(&["a", "b", "c"]));
        app.cursor = 2;
        app.editing_line = 1; // diverged from cursor
        app.input = "note on line 1".to_string();
        app.confirm_comment();
        assert!(app.comments.contains_key(&1));
        assert!(!app.comments.contains_key(&2));
    }

    // --- reload_file ---

    #[test]
    fn reload_file_updates_lines() {
        use tempfile::NamedTempFile;
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"alpha\nbeta\n").unwrap();
        let mut app = App::new_for_test(lines(&[]));
        app.file_path = f.path().to_string_lossy().into_owned();
        app.reload_file().unwrap();
        assert_eq!(app.lines, vec!["alpha", "beta"]);
    }

    #[test]
    fn reload_file_clamps_cursor_when_file_shrinks() {
        use tempfile::NamedTempFile;
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"only\n").unwrap();
        let mut app = App::new_for_test(lines(&["a", "b", "c", "d", "e"]));
        app.cursor = 4;
        app.file_path = f.path().to_string_lossy().into_owned();
        app.reload_file().unwrap();
        assert_eq!(app.cursor, 0); // file has 1 line → cursor clamped to 0
    }

    #[test]
    fn reload_file_adjusts_scroll_after_shrink() {
        use tempfile::NamedTempFile;
        let mut f = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"only\n").unwrap();
        let mut app = App::new_for_test(lines(&["a", "b", "c", "d", "e"]));
        app.cursor = 4;
        app.scroll = 4; // scroll was at the bottom of the old content
        app.view_height = 10;
        app.file_path = f.path().to_string_lossy().into_owned();
        app.reload_file().unwrap();
        // cursor clamped to 0, scroll must also be 0
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn reload_file_ok_when_file_deleted() {
        let mut app = App::new_for_test(lines(&["a", "b"]));
        app.file_path = "/tmp/tuitr_nonexistent_file_xyz".to_string();
        let result = app.reload_file();
        assert!(result.is_ok());
        // lines unchanged since file didn't exist
        assert_eq!(app.lines, vec!["a", "b"]);
    }
}
