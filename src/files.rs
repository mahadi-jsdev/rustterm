//! Lazy file tree for the sidebar's Files panel — one `read_dir` per
//! expand, dirs sorted before files, no recursion until asked.
//! Selection lives with the caller (`App::sidebar_sel`); methods take
//! and return row indices.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct FileRow {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    pub rows: Vec<FileRow>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> FileTree {
        let rows = read_children(&root, 0);
        FileTree { root, rows }
    }

    /// Enter on a dir expands/collapses it; on a file returns the path
    /// for the caller to open.
    pub fn activate(&mut self, sel: usize) -> Option<PathBuf> {
        let row = self.rows.get(sel)?;
        if row.is_dir {
            self.toggle_expand(sel);
            None
        } else {
            Some(row.path.clone())
        }
    }

    /// l/→ — expand a collapsed dir (no-op on files/expanded dirs).
    pub fn expand(&mut self, sel: usize) {
        if self
            .rows
            .get(sel)
            .map(|r| r.is_dir && !r.expanded)
            .unwrap_or(false)
        {
            self.toggle_expand(sel);
        }
    }

    /// h/← — collapse an expanded dir, else return the parent row (the
    /// nearest previous row one depth up) so the caller can move the
    /// selection there.
    pub fn collapse_or_parent(&mut self, sel: usize) -> usize {
        let Some(row) = self.rows.get(sel) else {
            return sel;
        };
        if row.is_dir && row.expanded {
            self.toggle_expand(sel);
            return sel;
        }
        if row.depth == 0 {
            return sel;
        }
        self.rows[..sel]
            .iter()
            .rposition(|r| r.depth == row.depth - 1)
            .unwrap_or(sel)
    }

    fn toggle_expand(&mut self, sel: usize) {
        let Some(row) = self.rows.get_mut(sel) else {
            return;
        };
        row.expanded = !row.expanded;
        if row.expanded {
            let kids = read_children(&row.path.clone(), row.depth + 1);
            self.rows.splice(sel + 1..sel + 1, kids);
        } else {
            let depth = self.rows[sel].depth;
            let mut end = sel + 1;
            while end < self.rows.len() && self.rows[end].depth > depth {
                end += 1;
            }
            self.rows.drain(sel + 1..end);
        }
    }
}

fn read_children(dir: &Path, depth: usize) -> Vec<FileRow> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                // .git internals are noise — never useful to browse here.
                .filter(|p| p.file_name().map(|n| n != ".git").unwrap_or(true))
                .collect()
        })
        .unwrap_or_default();
    // Dirs first, then alphabetical — case-insensitive like most
    // graphical file managers.
    paths.sort_by(|a, b| {
        b.is_dir().cmp(&a.is_dir()).then_with(|| {
            a.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .cmp(&b.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        })
    });
    paths
        .into_iter()
        .map(|path| FileRow {
            is_dir: path.is_dir(),
            path,
            depth,
            expanded: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree_fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustterm-tree-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::write(dir.join("src/main.rs"), "").unwrap();
        fs::write(dir.join("src/nested/deep.rs"), "").unwrap();
        fs::write(dir.join("b.txt"), "").unwrap();
        fs::write(dir.join("a.txt"), "").unwrap();
        dir
    }

    #[test]
    fn root_lists_dirs_first_then_files_sorted() {
        let dir = tree_fixture("root");
        let t = FileTree::new(dir.clone());
        let names: Vec<String> = t
            .rows
            .iter()
            .map(|r| r.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["src", "a.txt", "b.txt"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn expand_inserts_children_and_collapse_removes_them() {
        let dir = tree_fixture("expand");
        let mut t = FileTree::new(dir.clone());
        t.activate(0); // expand src/
        assert_eq!(t.rows.len(), 5); // src + nested + main.rs + a.txt + b.txt
        assert!(t.rows[0].expanded);
        assert_eq!(t.rows[1].path.file_name().unwrap(), "nested");
        assert_eq!(t.rows[1].depth, 1);
        t.activate(0); // collapse src/
        assert_eq!(t.rows.len(), 3);
        assert!(!t.rows[0].expanded);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn activate_file_returns_path_dir_returns_none() {
        let dir = tree_fixture("activate");
        let mut t = FileTree::new(dir.clone());
        assert_eq!(t.activate(1).unwrap().file_name().unwrap(), "a.txt");
        assert!(t.activate(0).is_none(), "dir toggles, no open path");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn collapse_or_parent_walks_up_the_tree() {
        let dir = tree_fixture("collapse");
        let mut t = FileTree::new(dir.clone());
        t.expand(0); // src/
        t.expand(1); // nested/
        assert_eq!(t.rows[2].depth, 2); // deep.rs
        assert_eq!(t.collapse_or_parent(2), 1); // file -> parent dir nested/
                                                // h on an expanded dir collapses it in place, sel stays.
        assert_eq!(t.collapse_or_parent(1), 1);
        assert_eq!(t.rows.len(), 5);
        assert_eq!(t.collapse_or_parent(1), 0); // collapsed dir -> parent src/
        t.collapse_or_parent(0); // collapses expanded src/
        assert_eq!(t.rows.len(), 3);
        let _ = fs::remove_dir_all(&dir);
    }
}
