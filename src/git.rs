use std::path::Path;
use std::process::Command;

pub struct GitStatus {
    pub branch: String,
    pub files: Vec<ChangedFile>,
}

pub struct ChangedFile {
    pub status: char,
    pub path: String,
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))
}

pub fn status(root: &Path) -> Option<GitStatus> {
    let out = git(root, &["status", "--porcelain=v1", "-z"]).ok()?;
    if !out.status.success() {
        return None;
    }
    Some(GitStatus {
        branch: branch_name(root),
        files: parse_porcelain(&out.stdout),
    })
}

fn branch_name(root: &Path) -> String {
    let named = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    named.unwrap_or_else(|| {
        git(root, &["rev-parse", "--short", "HEAD"])
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    })
}

/// porcelain -z: "XY path\0" per entry; renames emit a second nul
/// field holding the source path.
fn parse_porcelain(raw: &[u8]) -> Vec<ChangedFile> {
    let text = String::from_utf8_lossy(raw);
    let mut entries = text.split('\0').filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let x = entry.as_bytes()[0] as char;
        let y = entry.as_bytes()[1] as char;
        files.push(ChangedFile {
            status: display_letter(x, y),
            path: entry[3..].to_string(),
        });
        if x == 'R' || y == 'R' {
            entries.next(); // consume the rename source field
        }
    }
    files
}

fn display_letter(x: char, y: char) -> char {
    if x == '?' || y == '?' {
        '?'
    } else if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        'U'
    } else if x == 'R' || y == 'R' {
        'R'
    } else if x == 'A' {
        'A'
    } else if x == 'D' || y == 'D' {
        'D'
    } else {
        'M'
    }
}

pub fn branches(root: &Path) -> Vec<String> {
    let current = branch_name(root);
    let mut names: Vec<String> = git(root, &["branch", "--format=%(refname:short)"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|s| !s.is_empty() && s != &current)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut out = Vec::new();
    if !current.is_empty() {
        out.push(current);
    }
    out.extend(names);
    out
}

pub fn switch(root: &Path, branch: &str) -> Result<(), String> {
    let out = git(root, &["switch", branch])?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn add_all(root: &Path) -> Result<(), String> {
    let out = git(root, &["add", "-A"])?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn staged_diff(root: &Path) -> Result<String, String> {
    let out = git(root, &["diff", "--cached"])?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Returns the new commit's short hash.
pub fn commit(root: &Path, msg: &str) -> Result<String, String> {
    let out = git(root, &["commit", "-m", msg])?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(git(root, &["rev-parse", "--short", "HEAD"])
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    // Unique per test — parallel tests each create/remove their own repo.
    fn repo(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rustterm-git-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&root).args(args).output().unwrap()
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(root.join("a.txt"), b"one").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-m", "init"]);
        root
    }

    #[test]
    fn status_reports_branch_and_changes() {
        let root = repo("status");
        std::fs::write(root.join("a.txt"), b"two").unwrap();   // M
        std::fs::write(root.join("b.txt"), b"new").unwrap();    // ??
        let s = status(&root).unwrap();
        assert_eq!(s.branch, "main");
        assert!(s.files.iter().any(|f| f.path == "a.txt" && f.status == 'M'));
        assert!(s.files.iter().any(|f| f.path == "b.txt" && f.status == '?'));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn status_returns_none_outside_a_repo() {
        let root = std::env::temp_dir().join(format!("rustterm-git-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(status(&root).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn branches_lists_current_first_then_others() {
        let root = repo("branches");
        Command::new("git").arg("-C").arg(&root).args(["branch", "feature"]).output().unwrap();
        let bs = branches(&root);
        assert_eq!(bs[0], "main");
        assert!(bs.contains(&"feature".to_string()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn switch_and_staged_diff_and_commit_roundtrip() {
        let root = repo("switch");
        switch(&root, &branches(&root)[0]).unwrap(); // no-op switch succeeds
        std::fs::write(root.join("c.txt"), b"x").unwrap();
        add_all(&root).unwrap();
        assert!(staged_diff(&root).unwrap().contains("c.txt"));
        let hash = commit(&root, "test commit").unwrap();
        assert_eq!(hash.len(), 7);
        assert!(staged_diff(&root).unwrap().trim().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
