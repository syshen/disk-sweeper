use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use jwalk::{Parallelism, WalkDirGeneric};

use crate::tree::{NodeId, NodeKind, NodeMeta, Tree};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// 受 macOS TCC 保護、沒有「完全取用磁碟」權限時會讀取失敗的路徑（相對於 home）
const TCC_PATHS: &[&str] = &[
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Calendars",
    "Library/Reminders",
    "Library/Suggestions",
    ".Trash",
];

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub threads: usize,
    /// 小於此大小的檔案不建立節點，只計入父目錄
    pub min_node_size: u64,
}

#[derive(Debug, Clone)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
    pub permission_denied: bool,
}

impl ScanError {
    fn from_io(path: &Path, e: &io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            message: e.to_string(),
            permission_denied: e.kind() == io::ErrorKind::PermissionDenied,
        }
    }

    fn from_jwalk(e: &jwalk::Error) -> Self {
        let path = e.path().map(Path::to_path_buf).unwrap_or_default();
        match e.io_error() {
            Some(io_err) => Self::from_io(&path, io_err),
            None => Self { path, message: e.to_string(), permission_denied: false },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub files: u64,
    pub bytes: u64,
    pub errors: u64,
    pub current: PathBuf,
    /// 目前累計最大的第一層項目
    pub top: Vec<(String, u64)>,
}

#[derive(Debug)]
pub struct ScanResult {
    pub tree: Tree,
    pub errors: Vec<ScanError>,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum ScanEvent {
    Progress(Progress),
    Done(ScanResult),
}

#[derive(Debug, Clone, Copy)]
struct EntryMeta {
    kind: NodeKind,
    meta: NodeMeta,
    nlink: u64,
}

fn entry_meta(m: &std::fs::Metadata) -> EntryMeta {
    let ft = m.file_type();
    let kind = if ft.is_dir() {
        NodeKind::Dir
    } else if ft.is_file() {
        NodeKind::File
    } else if ft.is_symlink() {
        NodeKind::Symlink
    } else {
        NodeKind::Other
    };
    EntryMeta {
        kind,
        meta: NodeMeta { size: m.blocks() * 512, inode: m.ino(), dev: m.dev(), mtime: m.mtime() },
        nlink: m.nlink(),
    }
}

pub fn scan(root: &Path, opts: &ScanOptions, tx: &Sender<ScanEvent>) -> ScanResult {
    let start = Instant::now();
    let root_meta = match std::fs::symlink_metadata(root) {
        Ok(m) => entry_meta(&m),
        Err(e) => {
            return ScanResult {
                tree: Tree::new(root.to_path_buf(), NodeMeta::default()),
                errors: vec![ScanError::from_io(root, &e)],
                elapsed: start.elapsed(),
            };
        }
    };
    let root_dev = root_meta.meta.dev;
    let mut tree = Tree::new(root.to_path_buf(), root_meta.meta);
    let mut errors = Vec::new();
    let mut dirs: HashMap<PathBuf, NodeId> = HashMap::from([(root.to_path_buf(), tree.root())]);
    let mut hard_links: HashSet<(u64, u64)> = HashSet::new();
    let mut top: HashMap<String, u64> = HashMap::new();
    let mut progress = Progress::default();
    let mut last_emit = Instant::now();

    let walk = WalkDirGeneric::<((), Option<EntryMeta>)>::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(Parallelism::RayonNewPool(opts.threads.max(1)))
        .process_read_dir(move |_depth, _dir, _state, children| {
            // 在 rayon 執行緒上平行 lstat；不同檔案系統的目錄不往下走
            for child in children.iter_mut().flatten() {
                if let Ok(m) = std::fs::symlink_metadata(child.path()) {
                    let em = entry_meta(&m);
                    if em.kind == NodeKind::Dir && em.meta.dev != root_dev {
                        child.read_children_path = None;
                    }
                    child.client_state = Some(em);
                }
            }
        });

    for entry in walk {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors.push(ScanError::from_jwalk(&e));
                progress.errors += 1;
                continue;
            }
        };
        if entry.depth == 0 {
            continue;
        }
        let path = entry.path();
        if let Some(err) = &entry.read_children_error {
            errors.push(ScanError::from_jwalk(err));
            progress.errors += 1;
        }
        let Some(em) = entry.client_state else {
            errors.push(ScanError { path, message: "無法讀取 metadata".into(), permission_denied: false });
            progress.errors += 1;
            continue;
        };
        let Some(&parent) = dirs.get(&*entry.parent_path) else { continue };

        let mut meta = em.meta;
        if em.kind == NodeKind::File && em.nlink > 1 && !hard_links.insert((meta.dev, meta.inode)) {
            meta.size = 0;
        }
        let name = entry.file_name.to_string_lossy();
        if em.kind == NodeKind::Dir {
            let id = tree.add(parent, &name, em.kind, meta);
            dirs.insert(path.clone(), id);
        } else if meta.size < opts.min_node_size {
            tree.add_aggregate(parent, meta.size);
        } else {
            tree.add(parent, &name, em.kind, meta);
        }

        progress.files += 1;
        progress.bytes += meta.size;
        if let Some(first) = path.strip_prefix(root).ok().and_then(|r| r.components().next()) {
            let key = first.as_os_str().to_string_lossy();
            match top.get_mut(key.as_ref()) {
                Some(v) => *v += meta.size,
                None => {
                    top.insert(key.into_owned(), meta.size);
                }
            }
        }
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            last_emit = Instant::now();
            progress.current = path;
            progress.top = top_n(&top, 5);
            let _ = tx.send(ScanEvent::Progress(progress.clone()));
        }
    }

    tree.finalize();
    ScanResult { tree, errors, elapsed: start.elapsed() }
}

fn top_n(top: &HashMap<String, u64>, n: usize) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = top.iter().map(|(k, s)| (k.clone(), *s)).collect();
    v.sort_by_key(|a| std::cmp::Reverse(a.1));
    v.truncate(n);
    v
}

/// 是否有 TCC 保護路徑因權限不足而讀取失敗（需提示使用者授權「完全取用磁碟」）。
pub fn needs_full_disk_access(errors: &[ScanError], home: &Path) -> bool {
    errors.iter().filter(|e| e.permission_denied).any(|e| {
        e.path.strip_prefix(home).is_ok_and(|rel| TCC_PATHS.iter().any(|t| rel.starts_with(t)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use tempfile::TempDir;

    fn root() -> (TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().canonicalize().unwrap();
        (d, p)
    }

    fn write(p: &Path, len: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; len]).unwrap();
    }

    fn blocks(p: &Path) -> u64 {
        fs::symlink_metadata(p).unwrap().blocks() * 512
    }

    fn run(root: &Path, min_node_size: u64) -> ScanResult {
        let (tx, _rx) = mpsc::channel();
        scan(root, &ScanOptions { threads: 4, min_node_size }, &tx)
    }

    #[test]
    fn sums_allocated_sizes() {
        let (_d, r) = root();
        write(&r.join("a/x"), 10_000);
        write(&r.join("a/b/y"), 5_000);
        write(&r.join("z"), 1);
        let res = run(&r, 0);
        let expected: u64 = ["", "a", "a/b", "a/x", "a/b/y", "z"]
            .iter()
            .map(|rel| blocks(&if rel.is_empty() { r.clone() } else { r.join(rel) }))
            .sum();
        let t = &res.tree;
        assert_eq!(t.node(t.root()).size, expected);
        assert_eq!(t.node(t.root()).file_count, 3);
        let a = t.child_named(t.root(), "a").unwrap();
        assert_eq!(t.node(a).file_count, 2);
        assert!(res.errors.is_empty());
    }

    #[test]
    fn hard_links_are_counted_once() {
        let (_d, r) = root();
        write(&r.join("f"), 100_000);
        fs::hard_link(r.join("f"), r.join("g")).unwrap();
        let res = run(&r, 0);
        let t = &res.tree;
        assert_eq!(t.node(t.root()).size, blocks(&r) + blocks(&r.join("f")));
        assert_eq!(t.node(t.root()).file_count, 2);
    }

    #[test]
    fn permission_errors_are_recorded_and_scan_continues() {
        let (_d, r) = root();
        write(&r.join("locked/secret"), 10);
        write(&r.join("open/ok"), 10);
        fs::set_permissions(r.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let res = run(&r, 0);
        fs::set_permissions(r.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            res.errors.iter().any(|e| e.permission_denied && e.path.starts_with(r.join("locked"))),
            "{:?}",
            res.errors
        );
        assert!(res.tree.child_named(res.tree.root(), "open").is_some());
    }

    #[test]
    fn small_files_are_aggregated_into_parent() {
        let (_d, r) = root();
        write(&r.join("d/small"), 10);
        write(&r.join("d/big"), 100_000);
        let res = run(&r, 64 * 1024);
        let t = &res.tree;
        let d = t.child_named(t.root(), "d").unwrap();
        assert!(t.child_named(d, "small").is_none());
        assert!(t.child_named(d, "big").is_some());
        assert_eq!(t.node(d).file_count, 2);
        assert_eq!(
            t.node(d).size,
            blocks(&r.join("d")) + blocks(&r.join("d/small")) + blocks(&r.join("d/big"))
        );
    }

    #[test]
    fn symlinks_are_not_followed() {
        let (_d, r) = root();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("huge"), 200_000);
        std::os::unix::fs::symlink(outside.path(), r.join("link")).unwrap();
        let res = run(&r, 0);
        let t = &res.tree;
        let link = t.child_named(t.root(), "link").unwrap();
        assert_eq!(t.node(link).kind, NodeKind::Symlink);
        assert_eq!(t.node(t.root()).size, blocks(&r) + blocks(&r.join("link")));
    }

    #[test]
    fn missing_root_yields_error_not_panic() {
        let res = run(Path::new("/definitely/not/here"), 0);
        assert_eq!(res.errors.len(), 1);
        assert!(res.tree.is_empty());
    }

    #[test]
    fn detects_tcc_protected_failures() {
        let home = Path::new("/h");
        let err = |p: &str| ScanError { path: p.into(), message: "x".into(), permission_denied: true };
        assert!(needs_full_disk_access(&[err("/h/Library/Mail/V10")], home));
        assert!(!needs_full_disk_access(&[err("/h/Projects/x")], home));
    }
}
