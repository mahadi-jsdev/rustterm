use std::path::PathBuf;

/// Directory names matching the buffer's final segment — the candidates
/// shown as hints and used by `complete`. Only directories qualify (the
/// prompt only accepts dirs); dotfiles are hidden unless the tail starts
/// with `.`.
pub fn dir_candidates(buf: &str) -> Vec<String> {
    let (head, tail) = split(buf);
    let dir = if head.is_empty() {
        PathBuf::from(".")
    } else {
        crate::app::expand_tilde(&head)
    };
    let show_hidden = tail.starts_with('.');
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(tail) && (show_hidden || !n.starts_with('.')))
        .collect();
    names.sort();
    names
}

/// One Tab press: extend the buffer's tail toward the longest common
/// prefix of matching directories; a unique candidate gets a trailing
/// `/` so the next Tab descends into it. Returns whether the buffer
/// changed.
pub fn complete(buf: &mut String) -> bool {
    let (head, tail) = split(buf);
    match dir_candidates(buf).as_slice() {
        [] => false,
        [only] => {
            *buf = format!("{head}{only}/");
            true
        }
        cands => {
            let lcp = common_prefix(cands);
            if lcp.len() > tail.len() {
                *buf = format!("{head}{lcp}");
                true
            } else {
                false
            }
        }
    }
}

/// Longest common prefix of the candidates.
fn common_prefix(cands: &[String]) -> String {
    let Some(first) = cands.first() else {
        return String::new();
    };
    let mut p = first.clone();
    for c in &cands[1..] {
        while !c.starts_with(p.as_str()) {
            p.pop();
        }
    }
    p
}

/// (head, tail): head is everything up to and including the last `/`
/// (kept verbatim so `~/` stays unexpanded in the buffer), tail is the
/// segment being completed.
fn split(buf: &str) -> (String, &str) {
    match buf.rsplit_once('/') {
        Some((h, t)) => (format!("{h}/"), t),
        None => (String::new(), buf),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unique per test — tests run in parallel and each deletes its fixture.
    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rustterm-compl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["alpha", "alpine", "beta/sub", ".hidden"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("afile.txt"), b"").unwrap();
        root
    }

    #[test]
    fn candidates_match_prefix_dirs_only_sorted() {
        let root = fixture("cand");
        let c = dir_candidates(&format!("{}/al", root.display()));
        assert_eq!(c, vec!["alpha", "alpine"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hidden_dirs_only_match_when_tail_is_dotted() {
        let root = fixture("hidden");
        assert!(!dir_candidates(&format!("{}/", root.display())).contains(&".hidden".to_string()));
        assert_eq!(dir_candidates(&format!("{}/.", root.display())), vec![".hidden"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn complete_ambiguous_extends_to_common_prefix() {
        let root = fixture("amb");
        let mut buf = format!("{}/al", root.display());
        assert!(complete(&mut buf));
        assert_eq!(buf, format!("{}/alp", root.display()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn complete_unique_appends_slash_and_descends() {
        let root = fixture("uniq");
        let mut buf = format!("{}/bet", root.display());
        assert!(complete(&mut buf));
        assert_eq!(buf, format!("{}/beta/", root.display()));
        // Next Tab sees the trailing slash and completes inside beta/.
        assert!(complete(&mut buf));
        assert_eq!(buf, format!("{}/beta/sub/", root.display()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn complete_no_match_or_no_progress_leaves_buffer() {
        let root = fixture("nomatch");
        let mut buf = format!("{}/zzz", root.display());
        assert!(!complete(&mut buf));
        let mut buf2 = format!("{}/alp", root.display()); // already at lcp
        assert!(!complete(&mut buf2));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tilde_head_expands_for_lookup_but_stays_in_buffer() {
        // Completing inside ~/ keeps the ~ prefix verbatim.
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            return;
        }
        let marker = PathBuf::from(&home).join(format!("rustterm-compl-{}", std::process::id()));
        std::fs::create_dir_all(marker.join("xyz")).unwrap();
        assert_eq!(dir_candidates(&format!("~/{}/x", marker.file_name().unwrap().to_string_lossy())), vec!["xyz"]);
        std::fs::remove_dir_all(&marker).unwrap();
    }
}
