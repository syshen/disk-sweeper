pub mod journal;
pub mod procs;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::risk::Handling;
use crate::safety::ApprovedTarget;
use journal::{ActionLog, ActionRecord};

const MAX_DETAIL_CHARS: usize = 2000;

pub trait Trasher {
    fn trash(&self, path: &Path) -> Result<(), String>;
}

/// macOS 垃圾桶（trash crate 預設走 Finder，支援「放回原處」）
pub struct SystemTrash;

impl Trasher for SystemTrash {
    fn trash(&self, path: &Path) -> Result<(), String> {
        // `trash` crate（macOS 後端）canonicalize 路徑時只解析上層目錄，
        // 最後一個路徑元件本身不會被解析成連結目標，所以無論是走 Finder
        // (`osascript ... delete`) 還是 NSFileManager 的 `trashItemAtURL`，
        // 動作的對象都是傳入路徑本身（即符號連結），不會被解析成連結指向的
        // 目標。即便如此，這裡仍用 symlink_metadata 再次防呆：
        // 絕不把符號連結交給垃圾桶邏輯，一律拒絕並回報錯誤，也絕不退而
        // 改為直接刪除。
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return Err("無法安全地將符號連結移到垃圾桶".into());
            }
        }
        trash::delete(path).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    DryRun,
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ActionResult {
    pub path: PathBuf,
    pub size: u64,
    pub handling: Handling,
    pub outcome: Outcome,
    pub output: Option<String>,
}

/// 依序處理每個目標；單項失敗不影響其他項。非 dry-run 時沒有紀錄檔就全部拒絕。
pub fn execute(
    targets: &[ApprovedTarget],
    dry_run: bool,
    mut log: Option<&mut ActionLog>,
    trasher: &dyn Trasher,
) -> Vec<ActionResult> {
    let mut results = Vec::with_capacity(targets.len());
    for t in targets {
        let (outcome, output) = if !dry_run && log.is_none() {
            (Outcome::Failed("無法寫入操作紀錄，拒絕執行".into()), None)
        } else {
            run_one(t, dry_run, trasher)
        };
        let mut result =
            ActionResult { path: t.path().to_path_buf(), size: t.size(), handling: t.handling(), outcome, output };
        if let Some(log) = log.as_deref_mut() {
            if let Err(e) = log.write(&record_for(t, &result, dry_run)) {
                let note = format!("（操作紀錄寫入失敗：{e}）");
                result.output = Some(match result.output.take() {
                    Some(o) => format!("{o}\n{note}"),
                    None => note,
                });
            }
        }
        results.push(result);
    }
    results
}

fn run_one(t: &ApprovedTarget, dry_run: bool, trasher: &dyn Trasher) -> (Outcome, Option<String>) {
    if let Handling::Command(cmd) = t.handling() {
        if dry_run {
            return (Outcome::DryRun, Some(format!("將執行：{}", cmd.display())));
        }
        return match Command::new(cmd.program).args(cmd.args).output() {
            Ok(out) => {
                let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
                if out.status.success() {
                    (Outcome::Done, Some(text))
                } else {
                    (Outcome::Failed(format!("指令結束狀態：{}", out.status)), Some(text))
                }
            }
            Err(e) => (Outcome::Failed(e.to_string()), None),
        };
    }

    // 執行前重新 lstat：掃描後被改過或已消失就略過
    let meta = match std::fs::symlink_metadata(t.path()) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Outcome::Skipped("已不存在".into()), None),
        Err(e) => return (Outcome::Failed(e.to_string()), None),
    };
    if meta.ino() != t.inode() || meta.mtime() != t.mtime() {
        return (Outcome::Skipped("掃描後已變動".into()), None);
    }
    if dry_run {
        return (Outcome::DryRun, None);
    }
    let res = match t.handling() {
        // trash 失敗時只回報錯誤，絕不改為直接刪除
        Handling::Trash => trasher.trash(t.path()),
        // symlink_metadata 對 symlink 回傳 is_dir() == false，所以只會刪除連結本身
        Handling::Delete if meta.is_dir() => std::fs::remove_dir_all(t.path()).map_err(|e| e.to_string()),
        Handling::Delete => std::fs::remove_file(t.path()).map_err(|e| e.to_string()),
        Handling::Command(_) => unreachable!("handled above"),
    };
    match res {
        Ok(()) => (Outcome::Done, None),
        Err(e) => (Outcome::Failed(e), None),
    }
}

fn record_for(t: &ApprovedTarget, r: &ActionResult, dry_run: bool) -> ActionRecord {
    let (result, detail) = match &r.outcome {
        Outcome::Done | Outcome::DryRun => ("ok", r.output.clone().unwrap_or_default()),
        Outcome::Skipped(m) => ("skipped", m.clone()),
        Outcome::Failed(m) => ("error", m.clone()),
    };
    ActionRecord {
        ts: chrono::Local::now().to_rfc3339(),
        path: r.path.display().to_string(),
        size: r.size,
        handling: match r.handling {
            Handling::Delete => "delete",
            Handling::Trash => "trash",
            Handling::Command(_) => "command",
        },
        rule: t.rule_id(),
        result,
        detail: detail.chars().take(MAX_DETAIL_CHARS).collect(),
        dry_run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::risk::{Risk, ToolCommand};
    use crate::safety::{review, Candidate, Verdict};
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;

    #[derive(Default)]
    struct FakeTrash(RefCell<Vec<PathBuf>>);

    impl Trasher for FakeTrash {
        fn trash(&self, path: &Path) -> Result<(), String> {
            self.0.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
    }

    fn setup() -> (TempDir, Context) {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("Projects")).unwrap();
        let ctx = Context::new(d.path(), OsString::from("/usr/bin:/bin")).unwrap();
        (d, ctx)
    }

    fn approve(path: &Path, risk: Risk, rule: Option<&'static str>, command: Option<ToolCommand>, ctx: &Context) -> ApprovedTarget {
        let m = fs::symlink_metadata(path).unwrap();
        let c = Candidate {
            path: path.to_path_buf(),
            risk,
            rule_id: rule,
            command,
            size: m.blocks() * 512,
            inode: m.ino(),
            mtime: m.mtime(),
        };
        match review(c, ctx) {
            Verdict::Approved(t) => t,
            Verdict::Rejected { reason, .. } => panic!("{reason}"),
        }
    }

    fn log_in(d: &TempDir) -> ActionLog {
        ActionLog::open(&d.path().join("logs")).unwrap()
    }

    #[test]
    fn deletes_regenerable_dir_and_writes_log() {
        let (d, ctx) = setup();
        let target = ctx.home.join("Projects/web/node_modules");
        fs::create_dir_all(target.join("a")).unwrap();
        fs::write(target.join("a/i.js"), "x").unwrap();
        let t = approve(&target, Risk::Regenerable, Some("node-modules"), None, &ctx);
        let mut log = log_in(&d);
        let res = execute(&[t], false, Some(&mut log), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Done);
        assert!(!target.exists());
        let text = fs::read_to_string(d.path().join("logs/actions.jsonl")).unwrap();
        let rec: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(rec["handling"], "delete");
        assert_eq!(rec["result"], "ok");
        assert_eq!(rec["rule"], "node-modules");
        assert_eq!(rec["dry_run"], false);
    }

    #[test]
    fn user_data_goes_through_trasher() {
        let (d, ctx) = setup();
        let f = ctx.home.join("Projects/notes.txt");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::UserData, None, None, &ctx);
        let trash = FakeTrash::default();
        let res = execute(&[t], false, Some(&mut log_in(&d)), &trash);
        assert_eq!(res[0].outcome, Outcome::Done);
        assert_eq!(trash.0.borrow().as_slice(), [f.clone()]);
        assert!(f.exists(), "fake trash must not touch the file");
    }

    #[test]
    fn changed_after_scan_is_skipped() {
        let (d, ctx) = setup();
        let f = ctx.home.join("Projects/cache.bin");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        fs::remove_file(&f).unwrap();
        fs::write(&f, "new content").unwrap(); // 新檔案：inode 不同
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Skipped("掃描後已變動".into()));
        assert!(f.exists());
    }

    #[test]
    fn vanished_path_is_skipped() {
        let (d, ctx) = setup();
        let f = ctx.home.join("Projects/gone.bin");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        fs::remove_file(&f).unwrap();
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Skipped("已不存在".into()));
    }

    #[test]
    fn dry_run_changes_nothing_and_needs_no_log() {
        let (_d, ctx) = setup();
        let f = ctx.home.join("Projects/cache.bin");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        let res = execute(&[t], true, None, &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::DryRun);
        assert!(f.exists());
    }

    #[test]
    fn refuses_without_log_when_not_dry_run() {
        let (_d, ctx) = setup();
        let f = ctx.home.join("Projects/cache.bin");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        let res = execute(&[t], false, None, &FakeTrash::default());
        assert!(matches!(res[0].outcome, Outcome::Failed(_)));
        assert!(f.exists());
    }

    #[test]
    fn deleting_symlink_keeps_target() {
        let (d, ctx) = setup();
        let real = ctx.home.join("Projects/real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("keep.txt"), "x").unwrap();
        let link = ctx.home.join("Projects/link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let t = approve(&link, Risk::Regenerable, Some("r"), None, &ctx);
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Done);
        assert!(fs::symlink_metadata(&link).is_err());
        assert!(real.join("keep.txt").exists());
    }

    #[test]
    fn runs_tool_command_and_captures_output() {
        let (d, ctx) = setup();
        let dir = ctx.home.join("Projects/tool");
        fs::create_dir_all(&dir).unwrap();
        let cmd = ToolCommand { program: "echo", args: &["hello"] };
        let t = approve(&dir, Risk::ToolManaged, Some("t"), Some(cmd), &ctx);
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Done);
        assert!(res[0].output.as_deref().unwrap().contains("hello"));
        assert!(dir.exists());
    }

    #[test]
    fn tool_command_in_dry_run_is_not_executed() {
        let (_d, ctx) = setup();
        let dir = ctx.home.join("Projects/tool");
        fs::create_dir_all(&dir).unwrap();
        let cmd = ToolCommand { program: "echo", args: &["hello"] };
        let t = approve(&dir, Risk::ToolManaged, Some("t"), Some(cmd), &ctx);
        let res = execute(&[t], true, None, &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::DryRun);
        assert_eq!(res[0].output.as_deref(), Some("將執行：echo hello"));
    }
}
