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
        let Ok(mut child) = Command::new(cmd)
            .args(*args)
            .stdin(Stdio::piped())
            .spawn()
        else {
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

pub struct App {
    pub file_path: String,
    pub lines: Vec<String>,
    pub comments: HashMap<usize, String>,
    pub cursor: usize,
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

    fn move_down(&mut self) {
        if self.cursor + 1 < self.lines.len() {
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
        self.cursor = self.lines.len().saturating_sub(1);
        self.scroll_to_cursor();
    }

    fn scroll_to_cursor(&mut self) {
        if self.view_height == 0 {
            return;
        }
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

    fn start_comment(&mut self) {
        self.input = self.comments.get(&self.cursor).cloned().unwrap_or_default();
        self.mode = Mode::EditComment;
    }

    fn confirm_comment(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            self.comments.remove(&self.cursor);
            let _ = self.db.execute(
                "DELETE FROM comments WHERE file_path = ?1 AND line_number = ?2",
                params![self.file_path, self.cursor as i64],
            );
        } else {
            self.comments.insert(self.cursor, text.clone());
            let _ = self.db.execute(
                "INSERT OR REPLACE INTO comments (file_path, line_number, comment) VALUES (?1, ?2, ?3)",
                params![self.file_path, self.cursor as i64, text],
            );
        }
        self.input.clear();
        self.mode = Mode::Normal;
    }

    fn cancel_comment(&mut self) {
        self.input.clear();
        self.mode = Mode::Normal;
    }

    fn delete_comment(&mut self) {
        if self.comments.remove(&self.cursor).is_some() {
            let _ = self.db.execute(
                "DELETE FROM comments WHERE file_path = ?1 AND line_number = ?2",
                params![self.file_path, self.cursor as i64],
            );
            self.status = Some("Comment deleted".to_string());
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

    fn load_file(&mut self, path: String) -> Result<()> {
        if let Err(msg) = check_file_displayable(&path) {
            self.status = Some(msg);
            return Ok(());
        }

        if !self.file_path.is_empty() {
            let _ = self.watcher.unwatch(Path::new(&self.file_path));
        }
        let _ = self.watcher.watch(Path::new(&path), RecursiveMode::NonRecursive);

        let content = fs::read_to_string(&path)?;
        self.lines = content.lines().map(String::from).collect();
        self.comments = load_file_comments(&self.db, &path);
        self.file_path = path;
        self.cursor = 0;
        self.scroll = 0;
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
        self.rehighlight();
        self.compute_matches();
        self.status = Some("File reloaded".to_string());
        Ok(())
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
