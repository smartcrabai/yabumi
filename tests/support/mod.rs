//! Test helpers shared by `tests/samples.rs` (ARCHITECTURE.md §6.1/§6.2).

pub mod http_mock;
pub mod toml_lite;

use std::fs;
use std::path::{Path, PathBuf};

/// Enumerates every directory under `samples/` that has an `expected.toml`
/// (recursive search. As `SAMPLES_PLAN.md` §1.1 describes, `samples/` has a multi-level
/// subdirectory structure — `ok/`/`err/{static,lint,runtime,cli}/`/`fmt/`/`doctest/` — so the
/// walk is depth-independent and uses the presence of `expected.toml` as the only marker).
/// The return value is sorted into a deterministic order (lexicographic on the path string)
/// — `ReadDir`'s enumeration order is OS-dependent, and this keeps test execution order stable.
///
/// `root` accepts either a relative path like `samples` or an absolute path.
#[must_use]
pub fn discover_sample_dirs(root: &str) -> Vec<PathBuf> {
    let root_path = Path::new(root);
    let mut dirs = Vec::new();
    collect_dirs_with_expected_toml(root_path, &mut dirs);
    dirs.sort();
    dirs
}

fn collect_dirs_with_expected_toml(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut has_expected_toml = false;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("expected.toml") {
            has_expected_toml = true;
        }
    }
    if has_expected_toml {
        out.push(dir.to_path_buf());
    }
    for sub in subdirs {
        collect_dirs_with_expected_toml(&sub, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirms that `discover_sample_dirs` finds 89 directories under the real `samples/`
    /// (per the request: discover should find 89 directories). This is a unit test of the
    /// harness itself, so it is not marked `#[ignore]`.
    #[test]
    fn discovers_all_89_sample_directories() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let dirs = discover_sample_dirs(root.to_str().unwrap_or_default());
        assert_eq!(
            dirs.len(),
            89,
            "expected 89 directories but found {}: {:?}",
            dirs.len(),
            dirs
        );
        for dir in &dirs {
            assert!(
                dir.join("expected.toml").is_file(),
                "{} has no expected.toml",
                dir.display()
            );
        }
    }

    #[test]
    fn discovered_dirs_are_sorted_and_deduplicated() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let dirs = discover_sample_dirs(root.to_str().unwrap_or_default());
        let mut sorted = dirs.clone();
        sorted.sort();
        assert_eq!(dirs, sorted);
        let unique: std::collections::BTreeSet<_> = dirs.iter().collect();
        assert_eq!(unique.len(), dirs.len());
    }

    #[test]
    fn returns_empty_for_nonexistent_root() {
        let dirs = discover_sample_dirs("this/path/does/not/exist/at/all");
        assert!(dirs.is_empty());
    }
}
