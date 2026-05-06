use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;

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
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            entries: Vec::new(),
            cursor: 0,
            scroll: 0,
            expanded: HashSet::new(),
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
        let mut entries: Vec<_> = WalkBuilder::new(dir)
            .max_depth(Some(1))
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.depth() > 0)
            .collect();

        entries.sort_by(|a, b| {
            let a_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let b_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            b_dir.cmp(&a_dir).then(a.path().file_name().cmp(&b.path().file_name()))
        });

        for e in entries {
            let path = e.path().to_path_buf();
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
