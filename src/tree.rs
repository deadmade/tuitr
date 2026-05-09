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
            b_dir
                .cmp(&a_dir)
                .then(a.path().file_name().cmp(&b.path().file_name()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn tree(dir: &TempDir) -> FileTree {
        FileTree::new(dir.path().to_path_buf())
    }

    #[test]
    fn empty_dir_has_no_entries() {
        let dir = TempDir::new().unwrap();
        let t = tree(&dir);
        assert!(t.entries.is_empty());
        assert_eq!(t.cursor, 0);
    }

    #[test]
    fn dirs_sort_before_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("z_file.txt"), "").unwrap();
        fs::create_dir(dir.path().join("a_dir")).unwrap();
        let t = tree(&dir);
        assert_eq!(t.entries.len(), 2);
        assert!(t.entries[0].is_dir);
        assert!(!t.entries[1].is_dir);
    }

    #[test]
    fn files_sort_alphabetically() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("b.txt"), "").unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        let t = tree(&dir);
        assert_eq!(t.entries[0].path.file_name().unwrap(), "a.txt");
        assert_eq!(t.entries[1].path.file_name().unwrap(), "b.txt");
    }

    #[test]
    fn toggle_expand_shows_and_hides_children() {
        let dir = TempDir::new().unwrap();
        let subdir = dir.path().join("sub");
        fs::create_dir(&subdir).unwrap();
        fs::write(subdir.join("child.txt"), "").unwrap();
        let mut t = tree(&dir);
        assert_eq!(t.entries.len(), 1);
        assert!(!t.entries[0].is_expanded);

        t.toggle_expand();
        assert!(t.entries[0].is_expanded);
        assert_eq!(t.entries.len(), 2);

        t.toggle_expand();
        assert!(!t.entries[0].is_expanded);
        assert_eq!(t.entries.len(), 1);
    }

    #[test]
    fn current_file_none_on_dir() {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("mydir")).unwrap();
        let t = tree(&dir);
        assert!(t.current_file().is_none());
    }

    #[test]
    fn current_file_some_on_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("foo.txt"), "").unwrap();
        let t = tree(&dir);
        let path = t.current_file().unwrap();
        assert_eq!(path.file_name().unwrap(), "foo.txt");
    }

    #[test]
    fn move_down_bounded_at_last_entry() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("only.txt"), "").unwrap();
        let mut t = tree(&dir);
        assert_eq!(t.cursor, 0);
        t.move_down();
        assert_eq!(t.cursor, 0);
    }

    #[test]
    fn move_up_bounded_at_zero() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        let mut t = tree(&dir);
        t.move_up();
        assert_eq!(t.cursor, 0);
    }

    #[test]
    fn hidden_files_excluded() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".hidden"), "").unwrap();
        fs::write(dir.path().join("visible.txt"), "").unwrap();
        let t = tree(&dir);
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.entries[0].path.file_name().unwrap(), "visible.txt");
    }
}
