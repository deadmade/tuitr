use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

pub struct TreeEntry {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
    pub is_expanded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    pub entries: Vec<TreeEntry>,
    pub cursor: usize,
    pub scroll: usize,
    expanded: HashSet<PathBuf>,
    gitignore: Option<Gitignore>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let gitignore = build_gitignore(&root);
        let mut tree = Self {
            root: root.clone(),
            entries: Vec::new(),
            cursor: 0,
            scroll: 0,
            expanded: HashSet::new(),
            gitignore,
        };
        tree.rebuild();
        tree
    }

    pub fn rebuild(&mut self) {
        self.entries.clear();
        let root = self.root.clone();
        self.add_dir(&root, 0);
    }

    fn add_dir(&mut self, dir: &Path, depth: usize) {
        let Ok(read_dir) = fs::read_dir(dir) else {
            return;
        };

        let mut entries: Vec<_> = read_dir
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .filter(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                self.gitignore
                    .as_ref()
                    .map_or(true, |gi| !gi.matched(&e.path(), is_dir).is_ignore())
            })
            .collect();

        entries.sort_by(|a, b| {
            let a_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let b_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            b_dir.cmp(&a_dir).then(a.file_name().cmp(&b.file_name()))
        });

        for e in entries {
            let path = e.path();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let is_expanded = self.expanded.contains(&path);
            self.entries.push(TreeEntry {
                path: path.clone(),
                depth,
                is_dir,
                is_expanded,
            });
            if is_dir && is_expanded {
                self.add_dir(&path, depth + 1);
            }
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor + 1 < self.entries.len() {
            self.cursor += 1;
        }
    }

    pub fn toggle_expand(&mut self) {
        let Some(entry) = self.entries.get(self.cursor) else {
            return;
        };
        if !entry.is_dir {
            return;
        }
        let path = entry.path.clone();
        if self.expanded.contains(&path) {
            self.expanded.remove(&path);
        } else {
            self.expanded.insert(path.clone());
        }
        self.rebuild();
        if let Some(idx) = self.entries.iter().position(|e| e.path == path) {
            self.cursor = idx;
        }
    }

    pub fn current_file(&self) -> Option<&PathBuf> {
        self.entries
            .get(self.cursor)
            .filter(|e| !e.is_dir)
            .map(|e| &e.path)
    }

    pub fn scroll_to_cursor(&mut self, view_height: usize) {
        if view_height == 0 {
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + view_height {
            self.scroll = self.cursor - view_height + 1;
        }
    }
}

fn build_gitignore(root: &Path) -> Option<Gitignore> {
    let gi_path = root.join(".gitignore");
    if !gi_path.exists() {
        return None;
    }
    let mut builder = GitignoreBuilder::new(root);
    builder.add(gi_path);
    builder.build().ok()
}
