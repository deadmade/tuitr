use std::{collections::HashMap, fs, io, mem, path::Path, time::Duration};

use syntect::{
    easy::HighlightLines,
    highlighting::{Color as SyntectColor, Theme, ThemeSet},
    parsing::SyntaxSet,
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{tree::FileTree, ui};

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

    // Fallback for macOS / Windows
    arboard::Clipboard::new()?.set_text(text)?;
    Ok(())
}

pub enum Mode {
    Normal,
    EditComment,
}

pub enum Focus {
    Tree,
    File,
}

pub struct App {
    pub file_path: String,
    pub lines: Vec<String>,
    pub comments: HashMap<usize, String>,
    all_comments: HashMap<String, HashMap<usize, String>>,
    pub cursor: usize,
    pub scroll: usize,
    pub view_height: usize,
    pub view_width: u16,
    pub mode: Mode,
    pub focus: Focus,
    pub input: String,
    pub status: Option<String>,
    pub tree: FileTree,
    pub highlighted_lines: Vec<Vec<(SyntectColor, String)>>,
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl App {
    pub fn new(path: String) -> Result<Self> {
        let abs = fs::canonicalize(&path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.clone());

        let is_dir = Path::new(&abs).is_dir();

        let (root, file_path, lines) = if is_dir {
            (Path::new(&abs).to_path_buf(), String::new(), Vec::new())
        } else {
            let root = Path::new(&abs)
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();
            let content = fs::read_to_string(&abs)?;
            let lines = content.lines().map(String::from).collect();
            (root, abs, lines)
        };

        let syntax_set = SyntaxSet::load_defaults_nonewlines();
        let theme = ThemeSet::load_defaults().themes["base16-ocean.dark"].clone();

        let mut app = Self {
            file_path,
            lines,
            comments: HashMap::new(),
            all_comments: HashMap::new(),
            cursor: 0,
            scroll: 0,
            view_height: 20,
            view_width: 80,
            mode: Mode::Normal,
            focus: if is_dir { Focus::Tree } else { Focus::File },
            input: String::new(),
            status: None,
            tree: FileTree::new(root),
            highlighted_lines: Vec::new(),
            syntax_set,
            theme,
        };
        app.rehighlight();
        Ok(app)
    }

    pub fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        loop {
            terminal.draw(|f| {
                let area = f.area();
                let edit_h = if matches!(self.mode, Mode::EditComment) {
                    ui::comment_editor_height(&self.input, area.width)
                } else {
                    0
                };
                // top area height - borders (2) = inner view height
                self.view_height = area.height.saturating_sub(edit_h + 3) as usize;
                self.view_width = ui::file_area_width(area.width);
                self.tree.scroll_to_cursor(self.view_height);
                ui::render(f, self);
            })?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if self.handle_key(key) {
                        break;
                    }
                }
            }
        }

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        Ok(())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.status = None;

        if matches!(self.mode, Mode::EditComment) {
            self.handle_edit(key);
            return false;
        }

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Tab => self.toggle_focus(),
            _ => match self.focus {
                Focus::Tree => self.handle_tree(key),
                Focus::File => self.handle_file(key),
            },
        }
        false
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
        // Keep incrementing scroll until cursor fits in view_height display rows.
        loop {
            let rows: usize = (self.scroll..=self.cursor)
                .map(|i| {
                    1 + self
                        .comments
                        .get(&i)
                        .map(|comment| ui::comment_box_height(comment, self.view_width))
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
        } else {
            self.comments.insert(self.cursor, text);
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

    fn load_file(&mut self, path: String) -> Result<()> {
        let content = fs::read_to_string(&path)?;
        self.all_comments
            .insert(self.file_path.clone(), mem::take(&mut self.comments));
        self.lines = content.lines().map(String::from).collect();
        self.comments = self.all_comments.remove(&path).unwrap_or_default();
        self.file_path = path;
        self.cursor = 0;
        self.scroll = 0;
        self.rehighlight();
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
            let syntax = self.syntax_set
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
