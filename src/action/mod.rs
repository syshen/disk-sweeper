pub mod journal;
pub mod procs;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::docker;
use crate::risk::Handling;
use crate::safety::{ApprovedDocker, ApprovedTarget};
use crate::worktree;
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
        // 改為直接刪除。查不出 metadata（例如路徑已消失、權限問題）一律視為
        // 「無法確認安全性」而拒絕，不可放行給 `trash::delete`（fail closed）。
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                Err("無法安全地將符號連結移到垃圾桶".into())
            }
            Ok(_) => trash::delete(path).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        }
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

/// 紀錄檔無法寫入時，本項與之後所有項目一律拒絕執行（不可退而不記錄就繼續動手）。
const LOG_FAIL_MSG: &str = "無法寫入操作紀錄，拒絕執行";

/// 掃描完成後、真的動手前的檢查結果。
enum PreCheck {
    /// 不需要動手：略過、已消失、dry-run 或前置檢查本身出錯——直接是最終結果。
    Terminal(Outcome, Option<String>),
    /// 需要動手（刪除／丟垃圾桶／執行指令）。`is_dir` 只給 `Handling::Delete` 用。
    Proceed { is_dir: bool },
}

/// 依序處理每個目標；單項失敗不影響其他項尚未動手的檢查，
/// 但紀錄檔一旦寫不進去，本項與之後所有項目一律不動手、直接拒絕。
///
/// 每個真正會動手的目標，動手前都會先寫一筆 `result: "pending"` 的意圖紀錄；
/// 若這筆寫不進去，就完全不動手（避免「動了但沒留下紀錄」——例如磁碟滿了
/// 導致 `remove_dir_all` 執行到一半當掉，卻連要刪什麼都沒紀錄）。動手後才
/// 寫真正結果那筆紀錄，寫入失敗一樣視為紀錄檔失效、之後全部拒絕。
pub fn execute(
    targets: &[ApprovedTarget],
    dry_run: bool,
    mut log: Option<&mut ActionLog>,
    trasher: &dyn Trasher,
) -> Vec<ActionResult> {
    let mut results = Vec::with_capacity(targets.len());
    let mut halted = false;
    for t in targets {
        if halted || (!dry_run && log.is_none()) {
            halted = true;
            results.push(fail_result(t, LOG_FAIL_MSG));
            continue;
        }

        match precheck(t, dry_run) {
            PreCheck::Terminal(outcome, output) => {
                let mut result = to_result(t, outcome, output);
                if let Some(log) = log.as_deref_mut() {
                    if let Err(e) = log.write(&record_for(t, &result, dry_run)) {
                        append_log_failure_note(&mut result, e);
                        halted = true;
                    }
                }
                results.push(result);
            }
            PreCheck::Proceed { is_dir } => {
                if let Some(log) = log.as_deref_mut() {
                    if let Err(e) = log.write(&pending_record_for(t)) {
                        halted = true;
                        let mut result = fail_result(t, LOG_FAIL_MSG);
                        append_log_failure_note(&mut result, e);
                        results.push(result);
                        continue;
                    }
                }
                let (outcome, output) = perform(t, is_dir, trasher);
                let mut result = to_result(t, outcome, output);
                if let Some(log) = log.as_deref_mut() {
                    if let Err(e) = log.write(&record_for(t, &result, dry_run)) {
                        append_log_failure_note(&mut result, e);
                        halted = true;
                    }
                }
                results.push(result);
            }
        }
    }
    results
}

fn to_result(t: &ApprovedTarget, outcome: Outcome, output: Option<String>) -> ActionResult {
    ActionResult {
        path: t.path().to_path_buf(),
        size: t.size(),
        handling: t.handling(),
        outcome,
        output,
    }
}

fn fail_result(t: &ApprovedTarget, msg: &str) -> ActionResult {
    to_result(t, Outcome::Failed(msg.into()), None)
}

fn append_log_failure_note(result: &mut ActionResult, e: std::io::Error) {
    let note = format!("（操作紀錄寫入失敗：{e}）");
    result.output = Some(match result.output.take() {
        Some(o) => format!("{o}\n{note}"),
        None => note,
    });
}

/// 前置檢查：命令類只在 dry-run 時是終局（直接回報「將執行：...」）；
/// 刪除／垃圾桶類要先重新 lstat，掃描後被改過或已消失就略過，dry-run 也是終局。
fn precheck(t: &ApprovedTarget, dry_run: bool) -> PreCheck {
    if let Handling::Command(cmd) = t.handling() {
        if dry_run {
            return PreCheck::Terminal(Outcome::DryRun, Some(format!("將執行：{}", cmd.display())));
        }
        return PreCheck::Proceed { is_dir: false };
    }

    // 執行前重新 lstat：掃描後被改過或已消失就略過
    let meta = match std::fs::symlink_metadata(t.path()) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return PreCheck::Terminal(Outcome::Skipped("已不存在".into()), None)
        }
        Err(e) => return PreCheck::Terminal(Outcome::Failed(e.to_string()), None),
    };
    if meta.dev() != t.dev() || meta.ino() != t.inode() || meta.mtime() != t.mtime() {
        return PreCheck::Terminal(Outcome::Skipped("掃描後已變動".into()), None);
    }
    if dry_run {
        let note = (t.handling() == Handling::RemoveWorktree)
            .then(|| "將再次檢查後移到垃圾桶，並執行 git worktree prune".to_string());
        return PreCheck::Terminal(Outcome::DryRun, note);
    }
    // symlink_metadata 對 symlink 回傳 is_dir() == false，所以只會刪除連結本身
    PreCheck::Proceed {
        is_dir: meta.is_dir(),
    }
}

/// 真的動手：刪除、丟垃圾桶，或執行工具的官方指令。
fn perform(t: &ApprovedTarget, is_dir: bool, trasher: &dyn Trasher) -> (Outcome, Option<String>) {
    if let Handling::Command(cmd) = t.handling() {
        // 執行審核時檢查過的那個絕對路徑，不再重新查 PATH
        let Some(program) = t.program_path() else {
            return (Outcome::Failed("缺少已檢查的指令路徑".into()), None);
        };
        return match Command::new(program).args(cmd.args).output() {
            Ok(out) => {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                if out.status.success() {
                    (Outcome::Done, Some(text))
                } else {
                    (
                        Outcome::Failed(format!("指令結束狀態：{}", out.status)),
                        Some(text),
                    )
                }
            }
            Err(e) => (Outcome::Failed(e.to_string()), None),
        };
    }
    if t.handling() == Handling::RemoveWorktree {
        return remove_worktree(t, trasher);
    }
    let res = match t.handling() {
        // trash 失敗時只回報錯誤，絕不改為直接刪除
        Handling::Trash => trasher.trash(t.path()),
        Handling::Delete if is_dir => std::fs::remove_dir_all(t.path()).map_err(|e| e.to_string()),
        Handling::Delete => std::fs::remove_file(t.path()).map_err(|e| e.to_string()),
        Handling::Command(_) | Handling::RemoveWorktree | Handling::Docker => {
            unreachable!("handled above or never approved for a path")
        }
    };
    match res {
        Ok(()) => (Outcome::Done, None),
        Err(e) => (Outcome::Failed(e), None),
    }
}

/// 掃描後 worktree 可能又有新的修改或 commit：動手前用 git 重新檢查一次，
/// 仍可移除才移到垃圾桶，再到主 repo 執行 `git worktree prune`。
fn remove_worktree(t: &ApprovedTarget, trasher: &dyn Trasher) -> (Outcome, Option<String>) {
    let Some(git) = t.program_path() else {
        return (Outcome::Failed("缺少已檢查的 git 路徑".into()), None);
    };
    match worktree::inspect(t.path(), Some(git)) {
        Some((_, worktree::Status::Removable { .. })) => {}
        Some((_, worktree::Status::Keep(reason))) => return (Outcome::Skipped(reason), None),
        None => return (Outcome::Skipped("已不是 git worktree".into()), None),
    }
    let Some(common) = worktree::common_dir(t.path()) else {
        return (Outcome::Skipped("已不是 git worktree".into()), None);
    };
    // trash 失敗時只回報錯誤，絕不改為直接刪除
    if let Err(e) = trasher.trash(t.path()) {
        return (Outcome::Failed(e), None);
    }
    match worktree::prune(git, &common) {
        Ok(out) if out.is_empty() => (Outcome::Done, Some("已執行 git worktree prune".into())),
        Ok(out) => (Outcome::Done, Some(out)),
        Err(e) => (
            Outcome::Done,
            Some(format!("已移到垃圾桶，但 git worktree prune 失敗：{e}")),
        ),
    }
}

fn handling_str(h: Handling) -> &'static str {
    match h {
        Handling::Delete => "delete",
        Handling::Trash => "trash",
        Handling::Command(_) => "command",
        Handling::RemoveWorktree => "worktree",
        Handling::Docker => "docker",
    }
}

/// 移除 Docker 項目。和 `execute` 相同的原則：只接受審核過的型別、動手前先寫
/// 意圖紀錄、紀錄寫不進去就停手。
///
/// 動手前重查一次 Docker 狀態（所有項目共用同一次查詢），已不存在或變成使用中的
/// 物件不動；合併的匿名 volume 逐個移除，單一個失敗不影響其他的。
pub fn execute_docker(
    targets: &[ApprovedDocker],
    dry_run: bool,
    mut log: Option<&mut ActionLog>,
    runner: &dyn docker::Runner,
) -> Vec<ActionResult> {
    let mut results = Vec::with_capacity(targets.len());
    let mut halted = false;
    let mut fresh: Option<Result<Vec<docker::Item>, String>> = None;
    for t in targets {
        let item = t.item();
        let result = |outcome, output| ActionResult {
            path: PathBuf::from(item.label()),
            size: item.size,
            handling: Handling::Docker,
            outcome,
            output,
        };
        if halted || (!dry_run && log.is_none()) {
            halted = true;
            results.push(result(Outcome::Failed(LOG_FAIL_MSG.into()), None));
            continue;
        }

        // 不需要動手的結局：dry-run、重查失敗、重查後已不符合條件
        let terminal = if dry_run {
            Some((Outcome::DryRun, Some(dry_run_note(&item.object))))
        } else {
            let fresh = fresh.get_or_insert_with(|| {
                runner
                    .run(t.program(), &docker::DF_ARGS, docker::QUERY_TIMEOUT)
                    .and_then(|json| docker::classify(&json))
            });
            match fresh {
                Err(e) => Some((
                    Outcome::Failed(format!("無法重新檢查 Docker 狀態：{e}")),
                    None,
                )),
                Ok(fresh) => match docker::recheck(&item.object, fresh) {
                    Err(reason) => Some((Outcome::Skipped(reason), None)),
                    Ok(_) => None,
                },
            }
        };
        if let Some((outcome, output)) = terminal {
            let mut r = result(outcome, output);
            if let Some(log) = log.as_deref_mut() {
                let rec = docker_record(item.key(), item.size, &r.outcome, &r.output, dry_run);
                if let Err(e) = log.write(&rec) {
                    append_log_failure_note(&mut r, e);
                    halted = true;
                }
            }
            results.push(r);
            continue;
        }

        let Some(Ok(fresh)) = &fresh else {
            unreachable!("terminal covers the failed query")
        };
        let Ok(object) = docker::recheck(&item.object, fresh) else {
            unreachable!("terminal covers objects that no longer qualify")
        };
        let before = docker::commands(&item.object).len();
        let cmds = docker::commands(&object);
        let single = before == 1;
        let (mut done, mut failed, mut last_err, mut last_out) = (0usize, 0usize, None, None);
        let mut log_error = None;
        for (key, args) in &cmds {
            let size = if single { item.size } else { 0 };
            if let Some(log) = log.as_deref_mut() {
                let pending = docker_record(key.clone(), size, &Outcome::Done, &None, false);
                let pending = ActionRecord {
                    result: "pending",
                    ..pending
                };
                if let Err(e) = log.write(&pending) {
                    log_error = Some(e);
                    break;
                }
            }
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let (outcome, output) = match runner.run(t.program(), &argv, docker::REMOVE_TIMEOUT) {
                Ok(out) => {
                    done += 1;
                    (Outcome::Done, Some(out))
                }
                Err(e) => {
                    failed += 1;
                    last_err = Some(e.clone());
                    (Outcome::Failed(e), None)
                }
            };
            if let Some(log) = log.as_deref_mut() {
                if let Err(e) =
                    log.write(&docker_record(key.clone(), size, &outcome, &output, false))
                {
                    log_error = Some(e);
                    break;
                }
            }
            last_out = output;
        }

        let skipped = before - cmds.len();
        let outcome = match (&log_error, failed, last_err) {
            (Some(_), _, _) => Outcome::Failed(LOG_FAIL_MSG.into()),
            (None, 0, _) => Outcome::Done,
            (None, _, Some(e)) if single => Outcome::Failed(e),
            (None, n, e) => Outcome::Failed(format!(
                "{n} 個移除失敗：{}",
                e.unwrap_or_default().lines().next().unwrap_or_default()
            )),
        };
        let output = if single {
            last_out
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
        } else {
            let mut text = format!("已移除 {done} 個");
            if skipped > 0 {
                text.push_str(&format!("，略過 {skipped} 個（已不存在或正在使用中）"));
            }
            Some(text)
        };
        let mut r = result(outcome, output);
        if let Some(e) = log_error {
            append_log_failure_note(&mut r, e);
            halted = true;
        }
        results.push(r);
    }
    results
}

fn dry_run_note(object: &docker::Object) -> String {
    let cmds = docker::commands(object);
    match cmds.as_slice() {
        [] => "沒有要執行的指令".into(),
        [(_, args)] => format!("將執行：docker {}", args.join(" ")),
        [(_, args), ..] => format!(
            "將執行：docker {} <name>（逐個，共 {} 個）",
            args[..args.len() - 1].join(" "),
            cmds.len()
        ),
    }
}

fn docker_record(
    key: String,
    size: u64,
    outcome: &Outcome,
    output: &Option<String>,
    dry_run: bool,
) -> ActionRecord {
    let (result, detail) = match outcome {
        Outcome::Done | Outcome::DryRun => ("ok", output.clone().unwrap_or_default()),
        Outcome::Skipped(m) => ("skipped", m.clone()),
        Outcome::Failed(m) => ("error", m.clone()),
    };
    ActionRecord {
        ts: chrono::Local::now().to_rfc3339(),
        path: key,
        size,
        handling: handling_str(Handling::Docker),
        rule: Some(docker::RULE_ID),
        result,
        detail: detail.chars().take(MAX_DETAIL_CHARS).collect(),
        dry_run,
    }
}

/// 動手前先寫的意圖紀錄：只標記「即將對這個目標做什麼」，detail 留空。
fn pending_record_for(t: &ApprovedTarget) -> ActionRecord {
    ActionRecord {
        ts: chrono::Local::now().to_rfc3339(),
        path: t.path().display().to_string(),
        size: t.size(),
        handling: handling_str(t.handling()),
        rule: t.rule_id(),
        result: "pending",
        detail: String::new(),
        dry_run: false,
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
        handling: handling_str(r.handling),
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

    fn approve(
        path: &Path,
        risk: Risk,
        rule: Option<&'static str>,
        command: Option<ToolCommand>,
        ctx: &Context,
    ) -> ApprovedTarget {
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
        let mut lines = text.lines();
        let pending: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(pending["result"], "pending");
        assert_eq!(pending["path"], target.display().to_string());
        let rec: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(rec["handling"], "delete");
        assert_eq!(rec["result"], "ok");
        assert_eq!(rec["rule"], "node-modules");
        assert_eq!(rec["dry_run"], false);
        assert!(
            lines.next().is_none(),
            "expected exactly one pending line and one outcome line"
        );
    }

    #[test]
    fn worktree_is_rechecked_trashed_and_pruned() {
        use crate::worktree::{self, testutil};
        let d = tempfile::tempdir().unwrap();
        let ctx = Context::new(d.path(), std::env::var_os("PATH").unwrap()).unwrap();
        let base = ctx.home.join("Projects");
        fs::create_dir_all(&base).unwrap();
        let repo = testutil::repo(&base);
        let clean = testutil::add_worktree(&repo, &base, "clean");
        let dirty = testutil::add_worktree(&repo, &base, "dirty");
        let rule = Some(worktree::RULE_ID);
        let targets = [
            approve(&clean, Risk::Worktree, rule, None, &ctx),
            approve(&dirty, Risk::Worktree, rule, None, &ctx),
        ];
        // 審核之後才改的檔案（不影響目錄本身的 mtime），要靠動手前的 git 重新檢查擋下
        fs::write(dirty.join("README"), "changed").unwrap();

        let dry = execute(&targets, true, None, &FakeTrash::default());
        assert!(dry.iter().all(|r| r.outcome == Outcome::DryRun));

        let trash = FakeTrash::default();
        let mut log = log_in(&d);
        let res = execute(&targets, false, Some(&mut log), &trash);
        assert_eq!(res[0].outcome, Outcome::Done);
        assert!(matches!(&res[1].outcome, Outcome::Skipped(r) if r.contains("未 commit")));
        assert_eq!(*trash.0.borrow(), vec![clean.clone()]);
        let text = fs::read_to_string(d.path().join("logs/actions.jsonl")).unwrap();
        assert!(text.contains(r#""handling":"worktree""#));
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
        assert_eq!(trash.0.borrow().as_slice(), std::slice::from_ref(&f));
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
    fn different_device_after_review_is_skipped() {
        let (d, ctx) = setup();
        let f = ctx.home.join("Projects/cache.bin");
        fs::write(&f, "x").unwrap();
        let mut t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        t.set_dev_for_test(t.dev().wrapping_add(1));
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
        let cmd = ToolCommand {
            program: "echo",
            args: &["hello"],
        };
        let t = approve(&dir, Risk::ToolManaged, Some("t"), Some(cmd), &ctx);
        assert!(t.program_path().unwrap().is_absolute());
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
        let cmd = ToolCommand {
            program: "echo",
            args: &["hello"],
        };
        let t = approve(&dir, Risk::ToolManaged, Some("t"), Some(cmd), &ctx);
        let res = execute(&[t], true, None, &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::DryRun);
        assert_eq!(res[0].output.as_deref(), Some("將執行：echo hello"));
    }

    /// 永遠寫入失敗的假 writer，用來模擬紀錄檔壞掉（例如磁碟滿了）。
    struct FailingWriter;

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("模擬磁碟已滿"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn log_write_failure_halts_all_remaining_targets_untouched() {
        let (_d, ctx) = setup();
        let f1 = ctx.home.join("Projects/a.bin");
        let f2 = ctx.home.join("Projects/b.bin");
        fs::write(&f1, "x").unwrap();
        fs::write(&f2, "y").unwrap();
        let t1 = approve(&f1, Risk::Regenerable, Some("r"), None, &ctx);
        let t2 = approve(&f2, Risk::Regenerable, Some("r"), None, &ctx);
        let mut log = ActionLog::from_writer(FailingWriter);
        let trash = FakeTrash::default();
        let res = execute(&[t1, t2], false, Some(&mut log), &trash);
        assert_eq!(res[0].outcome, Outcome::Failed(LOG_FAIL_MSG.into()));
        assert!(
            res[0]
                .output
                .as_deref()
                .unwrap_or("")
                .contains("模擬磁碟已滿"),
            "pending-write io error must be reported: {:?}",
            res[0].output
        );
        assert_eq!(res[1].outcome, Outcome::Failed(LOG_FAIL_MSG.into()));
        assert!(
            f1.exists(),
            "first target must not be touched: pending record failed to write"
        );
        assert!(
            f2.exists(),
            "second target must not be touched: run halted after first failure"
        );
        assert!(trash.0.borrow().is_empty());
    }

    #[derive(Default)]
    struct FailingTrash;

    impl Trasher for FailingTrash {
        fn trash(&self, _path: &Path) -> Result<(), String> {
            Err("模擬垃圾桶失敗".into())
        }
    }

    #[test]
    fn trash_failure_is_reported_and_file_is_kept() {
        let (d, ctx) = setup();
        let f = ctx.home.join("Projects/notes.txt");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::UserData, None, None, &ctx);
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FailingTrash);
        assert!(matches!(res[0].outcome, Outcome::Failed(_)));
        assert!(
            f.exists(),
            "trash failure must not fall back to deleting the file"
        );
    }

    // ── Docker ──

    use crate::docker::testutil::{sample_json, FakeDocker, HEX_A, HEX_B, HEX_C};
    use crate::docker::{self, Object};
    use crate::safety::ApprovedDocker;

    fn approve_docker(object: Object, risk: Risk) -> ApprovedDocker {
        ApprovedDocker::for_test(
            docker::Item {
                object,
                size: 100,
                full_size: 100,
                risk,
                description: String::new(),
            },
            Path::new("/fake/docker"),
        )
    }

    fn anon(names: &[&str]) -> ApprovedDocker {
        approve_docker(
            Object::AnonVolumes {
                names: names.iter().map(|s| s.to_string()).collect(),
            },
            Risk::DockerData,
        )
    }

    fn alpine() -> ApprovedDocker {
        approve_docker(
            Object::Image {
                reference: "alpine:latest".into(),
            },
            Risk::Regenerable,
        )
    }

    fn log_lines(d: &TempDir) -> Vec<serde_json::Value> {
        fs::read_to_string(d.path().join("logs/actions.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn docker_dry_run_runs_nothing_and_shows_commands() {
        let fake = FakeDocker::with_df(vec![Ok(sample_json())]);
        let res = execute_docker(&[anon(&[HEX_A, HEX_B]), alpine()], true, None, &fake);
        assert!(fake.calls.borrow().is_empty());
        assert_eq!(res[0].outcome, Outcome::DryRun);
        assert_eq!(
            res[0].output.as_deref(),
            Some("將執行：docker volume rm <name>（逐個，共 2 個）")
        );
        assert_eq!(
            res[1].output.as_deref(),
            Some("將執行：docker image rm alpine:latest")
        );
        assert_eq!(res[1].handling, Handling::Docker);
    }

    #[test]
    fn docker_removes_each_object_after_one_recheck_without_force() {
        let d = tempfile::tempdir().unwrap();
        let fake = FakeDocker::with_df(vec![Ok(sample_json())]);
        let mut log = log_in(&d);
        let res = execute_docker(
            &[anon(&[HEX_A, HEX_B]), alpine()],
            false,
            Some(&mut log),
            &fake,
        );
        assert_eq!(res[0].outcome, Outcome::Done);
        assert_eq!(res[0].output.as_deref(), Some("已移除 2 個"));
        assert_eq!(res[0].path, PathBuf::from("匿名 volume × 2"));
        assert_eq!(res[1].outcome, Outcome::Done);
        let calls = fake.calls.borrow().clone();
        assert_eq!(calls[0], docker::DF_ARGS);
        assert_eq!(
            fake.removals(),
            vec![
                vec!["volume", "rm", HEX_A],
                vec!["volume", "rm", HEX_B],
                vec!["image", "rm", "alpine:latest"],
            ]
        );
        assert_eq!(calls.len(), 4, "狀態只重查一次");
        assert!(calls.iter().flatten().all(|a| a != "-f" && a != "--force"));

        // 每個物件一筆 pending、一筆結果
        let lines = log_lines(&d);
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0]["result"], "pending");
        assert_eq!(lines[0]["path"], format!("docker:volume:{HEX_A}"));
        assert_eq!(lines[1]["result"], "ok");
        assert_eq!(lines[1]["handling"], "docker");
        assert_eq!(lines[1]["rule"], "docker-cleanup");
        assert_eq!(lines[5]["path"], "docker:image:alpine:latest");
    }

    #[test]
    fn docker_objects_that_became_used_are_left_alone() {
        let d = tempfile::tempdir().unwrap();
        // HEX_C 在樣本裡有 container 在用；odoo:18 也是
        let fake = FakeDocker::with_df(vec![Ok(sample_json())]);
        let used_image = approve_docker(
            Object::Image {
                reference: "odoo:18".into(),
            },
            Risk::Regenerable,
        );
        let res = execute_docker(
            &[anon(&[HEX_A, HEX_C]), anon(&[HEX_C]), used_image],
            false,
            Some(&mut log_in(&d)),
            &fake,
        );
        assert_eq!(res[0].outcome, Outcome::Done);
        assert_eq!(
            res[0].output.as_deref(),
            Some("已移除 1 個，略過 1 個（已不存在或正在使用中）")
        );
        assert!(matches!(res[1].outcome, Outcome::Skipped(_)));
        assert!(matches!(res[2].outcome, Outcome::Skipped(_)));
        assert_eq!(fake.removals(), vec![vec!["volume", "rm", HEX_A]]);
        assert_eq!(log_lines(&d).last().unwrap()["result"], "skipped");
    }

    #[test]
    fn docker_one_failed_removal_does_not_stop_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let mut fake = FakeDocker::with_df(vec![Ok(sample_json())]);
        fake.fail = vec![HEX_A.to_string()];
        let res = execute_docker(
            &[anon(&[HEX_A, HEX_B])],
            false,
            Some(&mut log_in(&d)),
            &fake,
        );
        assert!(matches!(&res[0].outcome, Outcome::Failed(m) if m.contains("1 個移除失敗")));
        assert_eq!(res[0].output.as_deref(), Some("已移除 1 個"));
        assert_eq!(fake.removals().len(), 2);
        let lines = log_lines(&d);
        assert_eq!(lines[1]["result"], "error");
        assert_eq!(lines[3]["result"], "ok");
    }

    #[test]
    fn docker_failed_recheck_removes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let fake = FakeDocker::with_df(vec![Err("Cannot connect".into())]);
        let res = execute_docker(
            &[anon(&[HEX_A]), alpine()],
            false,
            Some(&mut log_in(&d)),
            &fake,
        );
        for r in &res {
            assert!(matches!(&r.outcome, Outcome::Failed(m) if m.contains("無法重新檢查")));
        }
        assert!(fake.removals().is_empty());
    }

    #[test]
    fn docker_refuses_without_a_working_log() {
        let fake = FakeDocker::with_df(vec![Ok(sample_json())]);
        let res = execute_docker(&[alpine()], false, None, &fake);
        assert_eq!(res[0].outcome, Outcome::Failed(LOG_FAIL_MSG.into()));
        assert!(fake.calls.borrow().is_empty());

        // pending 寫不進去：一個都不移除，之後的項目也不動
        let mut log = ActionLog::from_writer(FailingWriter);
        let res = execute_docker(
            &[anon(&[HEX_A, HEX_B]), alpine()],
            false,
            Some(&mut log),
            &fake,
        );
        assert_eq!(res[0].outcome, Outcome::Failed(LOG_FAIL_MSG.into()));
        assert_eq!(res[1].outcome, Outcome::Failed(LOG_FAIL_MSG.into()));
        assert!(fake.removals().is_empty());
    }
}
