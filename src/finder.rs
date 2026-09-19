use std::path::{Path, PathBuf};

const MAX_FILES: usize = 10_000;
const MAX_RESULTS: usize = 50;

pub struct FileIndex {
    pub root: PathBuf,
    pub files: Vec<PathBuf>, // relative paths
}

/// Walk `root` respecting .gitignore/.git-exclude, skipping hidden files
/// and .git itself, capped at MAX_FILES.
pub fn build(root: &Path) -> FileIndex {
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(root).build().flatten() {
        if files.len() >= MAX_FILES {
            break;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(root) {
            files.push(rel.to_path_buf());
        }
    }
    files.sort();
    FileIndex { root: root.to_path_buf(), files }
}

/// Fuzzy-filter relative paths, best score first, top MAX_RESULTS.
/// Empty query → all files in index order (fuzzy_score(“”, _) = 0).
pub fn filter<'a>(index: &'a FileIndex, query: &str) -> Vec<&'a Path> {
    let mut scored: Vec<(f64, &'a Path)> = index
        .files
        .iter()
        .filter_map(|p| {
            crate::palette::fuzzy_score(query, &p.to_string_lossy()).map(|s| (s, p.as_path()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(MAX_RESULTS).map(|(_, p)| p).collect()
}

/// Finder UI state — same shape as `palette::Palette`.
pub struct FinderState {
    pub index: FileIndex,
    pub query: String,
    pub selected: usize,
}

impl FinderState {
    pub fn open(root: &Path) -> FinderState {
        FinderState { index: build(root), query: String::new(), selected: 0 }
    }

    pub fn filtered(&self) -> Vec<&Path> {
        filter(&self.index, &self.query)
    }

    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.selected = 0;
    }

    pub fn move_next(&mut self) {
        let n = self.filtered().len();
        if n > 0 {
            self.selected = (self.selected + 1) % n;
        }
    }

    pub fn move_prev(&mut self) {
        let n = self.filtered().len();
        if n > 0 {
            self.selected = if self.selected == 0 { n - 1 } else { self.selected - 1 };
        }
    }

    /// Absolute path of the current selection.
    pub fn selected_path(&self) -> Option<PathBuf> {
        self.filtered()
            .get(self.selected)
            .map(|p| self.index.root.join(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rustterm-find-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for f in ["src/main.rs", "src/lib.rs", "docs/spec.md", "ignored.log"] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), b"x").unwrap();
        std::fs::write(root.join(".hidden"), b"x").unwrap();
        std::fs::write(root.join(".gitignore"), b"ignored.log\n").unwrap();
        root
    }

    #[test]
    fn build_skips_gitignored_hidden_and_dotgit() {
        let root = tree("build");
        // .gitignore only applies inside a git repo — init one.
        std::process::Command::new("git").arg("-C").arg(&root).args(["init", "-q"]).output().unwrap();
        let idx = build(&root);
        let names: Vec<String> = idx.files.iter().map(|p| p.to_string_lossy().to_string()).collect();
        assert!(names.contains(&"src/main.rs".into()));
        assert!(!names.iter().any(|n| n.contains("ignored.log")));
        assert!(!names.iter().any(|n| n.starts_with(".git") || n.contains(".git/")));
        assert!(!names.contains(&".hidden".to_string()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn filter_fuzzy_ranks_and_caps() {
        let root = tree("filter");
        let idx = build(&root);
        let hits = filter(&idx, "main");
        assert_eq!(hits[0].to_string_lossy(), "src/main.rs");
        // Empty query returns everything (alphabetical), capped at 50.
        assert_eq!(filter(&idx, "").len(), idx.files.len().min(50));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn state_nav_wraps_and_tracks_selection() {
        let root = tree("state");
        let mut s = FinderState::open(&root);
        assert_eq!(s.selected, 0);
        s.move_prev();
        assert_eq!(s.selected, s.filtered().len().saturating_sub(1).min(49));
        s.move_next();
        assert_eq!(s.selected, 0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
