//! Git worktree 檢查：判斷哪些 linked worktree 可以安全移除。
//!
//! linked worktree 的根目錄有一個 `.git` **檔案**（不是目錄），內容是
//! `gitdir: <主 repo>/.git/worktrees/<name>`。submodule 也是 `.git` 檔案，
//! 但它指向的目錄沒有 `commondir`，以此區分。
//!
//! 可以移除的條件（全部成立）：
//! - 沒有被 `git worktree lock` 鎖定
//! - 沒有進行中的 merge / rebase / cherry-pick / revert / bisect
//! - `git status` 乾淨：沒有未 commit 的修改，也沒有未追蹤的檔案（被 ignore 的不算）
//! - 目前的 commit 也包含在**其他** ref（別的 branch、remote branch 或 tag）裡，
//!   移除後不會遺失任何 commit
//!
//! 移除方式是移到垃圾桶再 `git worktree prune`；branch 一律保留。
//!
//! 安全：掃描會對 `$HOME` 下找到的每個 worktree 自動執行 git，而 repo 自己的設定
//! （例如解壓縮來的可疑目錄）可以讓 git 執行任意指令。因此每次呼叫 git 都關掉
//! `core.fsmonitor` 與 hooks；repo 層級設定了 filter driver（`git status` 會執行
//! clean filter）或含 submodule 的一律保留、不執行 `git status`。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::tree::NodeId;

pub const RULE_ID: &str = "git-worktree";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// 可以移除；`kept_in` 是也包含目前 commit 的其他 ref（簡短名稱）
    Removable { kept_in: String },
    /// 保留，附原因
    Keep(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub node: NodeId,
    /// 目前 checkout 的 branch；detached HEAD 為 None
    pub branch: Option<String>,
    pub status: Status,
}

impl Worktree {
    pub fn removable(&self) -> bool {
        matches!(self.status, Status::Removable { .. })
    }

    pub fn description(&self) -> String {
        let head = match &self.branch {
            Some(b) => format!("branch {b}"),
            None => "detached HEAD".to_string(),
        };
        match &self.status {
            Status::Removable { kept_in } => format!(
                "{head}，工作目錄乾淨，commit 也在 {kept_in} · 移到垃圾桶並執行 git worktree prune（branch 保留）"
            ),
            Status::Keep(reason) => format!("{head} · 保留：{reason}"),
        }
    }
}

/// worktree 與主 repo 的連結
struct Link {
    /// `<主 repo>/.git/worktrees/<name>`
    admin: PathBuf,
    /// 主 repo 的 `.git`
    common: PathBuf,
}

/// 讀 `<dir>/.git` 檔案；不是 linked worktree（submodule、壞掉的連結、一般 repo）回傳 None
fn link(dir: &Path) -> Option<Link> {
    let dotgit = dir.join(".git");
    if !fs::symlink_metadata(&dotgit).ok()?.is_file() {
        return None;
    }
    let text = fs::read_to_string(&dotgit).ok()?;
    let gitdir = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    // gitdir 可能是相對路徑（相對於 worktree 根目錄）；絕對路徑 join 後會直接取代
    let admin = dir.join(gitdir);
    let common = admin.join(fs::read_to_string(admin.join("commondir")).ok()?.trim());
    // 正常的 worktree 管理目錄一定是 <common>/worktrees/<name>
    if admin.canonicalize().ok()?.parent()? != common.canonicalize().ok()?.join("worktrees") {
        return None;
    }
    // admin/gitdir 必須指回這個 worktree 的 .git，兩邊對得上才算
    let back = fs::read_to_string(admin.join("gitdir")).ok()?;
    let back = admin.join(back.trim()).canonicalize().ok()?;
    if back != dotgit.canonicalize().ok()? {
        return None;
    }
    Some(Link { admin, common })
}

/// 主 repo 的 `.git` 目錄；不是 linked worktree 回傳 None
pub fn common_dir(dir: &Path) -> Option<PathBuf> {
    link(dir).map(|l| l.common)
}

/// 關掉 repo 設定能觸發的外部指令（fsmonitor、hooks），也不會跳出任何提示
fn git_cmd(git: &Path) -> Command {
    let mut cmd = Command::new(git);
    cmd.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
    ])
    // 只讀取，不要讓 git status 順手更新 index
    .env("GIT_OPTIONAL_LOCKS", "0")
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("LC_ALL", "C");
    cmd
}

fn git_out(git: &Path, dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_cmd(git)
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().next().unwrap_or("").trim().to_string())
    }
}

/// 檢查 `dir` 這個 worktree；不是 linked worktree 回傳 None。
/// `git` 是 git 執行檔的絕對路徑，找不到 git 時傳 None（一律保留）。
pub fn inspect(dir: &Path, git: Option<&Path>) -> Option<(Option<String>, Status)> {
    let link = link(dir)?;
    let Some(git) = git else {
        return Some((None, Status::Keep("找不到 git 指令，無法檢查".into())));
    };
    let branch = git_out(git, dir, &["symbolic-ref", "-q", "--short", "HEAD"])
        .ok()
        .filter(|b| !b.is_empty());
    let status = check(dir, git, &link, branch.as_deref());
    Some((branch, status))
}

fn check(dir: &Path, git: &Path, link: &Link, branch: Option<&str>) -> Status {
    if link.admin.join("locked").exists() {
        return Status::Keep("已被 git worktree lock 鎖定".into());
    }
    for (file, op) in [
        ("MERGE_HEAD", "merge"),
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
        ("BISECT_LOG", "bisect"),
    ] {
        if link.admin.join(file).exists() {
            return Status::Keep(format!("有進行中的 {op}"));
        }
    }
    if dir.join(".gitmodules").exists() {
        return Status::Keep("含 submodule，請手動處理".into());
    }
    // git status 會執行 filter driver 的 clean 指令：只信任使用者自己的
    // global / system 設定（例如 git-lfs），repo 自己設定的一律不執行
    match git_out(
        git,
        dir,
        &["config", "--show-scope", "--get-regexp", r"^filter\."],
    ) {
        Ok(lines) => {
            if lines
                .lines()
                .any(|l| !(l.starts_with("global\t") || l.starts_with("system\t")))
            {
                return Status::Keep("repo 自己設定了 filter driver，為安全起見不自動檢查".into());
            }
        }
        // exit 1 且沒有錯誤訊息：沒有任何 filter 設定
        Err(e) if e.is_empty() => {}
        Err(e) => return Status::Keep(format!("無法讀取 git 設定：{e}")),
    }
    match git_out(
        git,
        dir,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ],
    ) {
        Err(e) => return Status::Keep(format!("無法執行 git status：{e}")),
        Ok(s) if !s.is_empty() => return Status::Keep("有未 commit 的修改或未追蹤的檔案".into()),
        Ok(_) => {}
    }
    let refs = match git_out(
        git,
        dir,
        &["for-each-ref", "--contains", "HEAD", "--format=%(refname)"],
    ) {
        Ok(r) => r,
        Err(e) => return Status::Keep(format!("無法確認 commit 是否在其他 branch：{e}")),
    };
    let own = branch.map(|b| format!("refs/heads/{b}"));
    // refs/stash 的 commit 以 HEAD 為 parent，一定「包含」HEAD，不能算數
    match refs
        .lines()
        .find(|r| Some(*r) != own.as_deref() && *r != "refs/stash")
    {
        Some(r) => Status::Removable {
            kept_in: short_ref(r).to_string(),
        },
        None => {
            Status::Keep("有 commit 只在這個 worktree 的 branch 上（尚未 merge 或 push）".into())
        }
    }
}

fn short_ref(r: &str) -> &str {
    ["refs/heads/", "refs/remotes/", "refs/tags/"]
        .iter()
        .find_map(|p| r.strip_prefix(p))
        .unwrap_or(r)
}

/// 在主 repo 執行 `git worktree prune`，清掉已不存在的 worktree 紀錄
pub fn prune(git: &Path, common: &Path) -> Result<String, String> {
    let out = git_cmd(git)
        .arg("--git-dir")
        .arg(common)
        .args(["worktree", "prune", "--verbose"])
        .output()
        .map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text.trim().to_string())
    } else {
        Err(text.trim().to_string())
    }
}

#[cfg(test)]
pub mod testutil {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub fn git_bin() -> PathBuf {
        crate::safety::find_in_path("git", &std::env::var_os("PATH").unwrap_or_default())
            .expect("tests need git")
    }

    pub fn git(dir: &Path, args: &[&str]) {
        let out = Command::new(git_bin())
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 在 `base/repo` 建一個有一個 commit 的 repo（branch main），回傳 repo 路徑
    pub fn repo(base: &Path) -> PathBuf {
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "hi").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored/\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        repo
    }

    /// 從 repo 開一個新 branch 的 worktree 到 `base/<name>`
    pub fn add_worktree(repo: &Path, base: &Path, name: &str) -> PathBuf {
        let wt = base.join(name);
        git(
            repo,
            &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()],
        );
        wt
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    fn status(dir: &Path) -> Option<Status> {
        inspect(dir, Some(&git_bin())).map(|(_, s)| s)
    }

    fn base() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().canonicalize().unwrap();
        (d, p)
    }

    #[test]
    fn fresh_worktree_at_main_commit_is_removable() {
        let (_d, base) = base();
        let repo = repo(&base);
        let wt = add_worktree(&repo, &base, "feat");
        // 被 ignore 的檔案不影響判斷
        fs::create_dir_all(wt.join("ignored")).unwrap();
        fs::write(wt.join("ignored/cache"), "x").unwrap();
        let (branch, st) = inspect(&wt, Some(&git_bin())).unwrap();
        assert_eq!(branch.as_deref(), Some("feat"));
        assert_eq!(
            st,
            Status::Removable {
                kept_in: "main".into()
            }
        );
        assert_eq!(
            common_dir(&wt).unwrap().canonicalize().unwrap(),
            repo.join(".git")
        );
    }

    #[test]
    fn unique_commits_untracked_and_modified_files_are_kept() {
        let (_d, base) = base();
        let repo = repo(&base);

        let wt = add_worktree(&repo, &base, "untracked");
        fs::write(wt.join("new.txt"), "x").unwrap();
        assert!(matches!(status(&wt), Some(Status::Keep(r)) if r.contains("未追蹤")));

        let wt = add_worktree(&repo, &base, "modified");
        fs::write(wt.join("README"), "changed").unwrap();
        assert!(matches!(status(&wt), Some(Status::Keep(_))));

        let wt = add_worktree(&repo, &base, "ahead");
        fs::write(wt.join("f"), "x").unwrap();
        git(&wt, &["add", "f"]);
        git(&wt, &["commit", "-q", "-m", "wip"]);
        assert!(matches!(status(&wt), Some(Status::Keep(r)) if r.contains("尚未 merge")));
        // stash 不算「其他 ref」
        fs::write(wt.join("f"), "y").unwrap();
        git(&wt, &["stash", "-q"]);
        assert!(matches!(status(&wt), Some(Status::Keep(r)) if r.contains("尚未 merge")));
        // merge 進 main 之後就可以移除
        git(&repo, &["merge", "-q", "--ff-only", "ahead"]);
        assert_eq!(
            status(&wt),
            Some(Status::Removable {
                kept_in: "main".into()
            })
        );
    }

    #[test]
    fn locked_worktree_is_kept() {
        let (_d, base) = base();
        let repo = repo(&base);
        let wt = add_worktree(&repo, &base, "locked");
        git(&repo, &["worktree", "lock", wt.to_str().unwrap()]);
        assert!(matches!(status(&wt), Some(Status::Keep(r)) if r.contains("鎖定")));
    }

    #[test]
    fn detached_head_is_removable_when_commit_is_elsewhere() {
        let (_d, base) = base();
        let repo = repo(&base);
        let wt = base.join("detached");
        git(
            &repo,
            &["worktree", "add", "-q", "--detach", wt.to_str().unwrap()],
        );
        let (branch, st) = inspect(&wt, Some(&git_bin())).unwrap();
        assert_eq!(branch, None);
        assert!(matches!(st, Status::Removable { .. }));
    }

    #[test]
    fn main_repo_and_non_worktree_git_files_are_ignored() {
        let (_d, base) = base();
        let repo = repo(&base);
        assert_eq!(status(&repo), None);
        // 像 submodule 那樣指向沒有 commondir 的目錄
        let sub = base.join("sub");
        fs::create_dir_all(repo.join(".git/modules/sub")).unwrap();
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/modules/sub").display()),
        )
        .unwrap();
        assert_eq!(status(&sub), None);
    }

    #[test]
    fn repo_controlled_commands_are_never_run() {
        let (_d, base) = base();
        let repo = repo(&base);
        let wt = add_worktree(&repo, &base, "evil");
        let marker = base.join("pwned");
        let script = base.join("evil.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\ntouch {}\ncat\n", marker.display()),
        )
        .unwrap();
        Command::new("chmod")
            .arg("755")
            .arg(&script)
            .status()
            .unwrap();
        let s = script.to_str().unwrap();

        // fsmonitor 被 -c 覆寫掉，不會執行
        git(&repo, &["config", "core.fsmonitor", s]);
        assert!(matches!(status(&wt), Some(Status::Removable { .. })));
        assert!(!marker.exists(), "fsmonitor must not run");

        // repo 層級的 filter driver：不執行 git status，直接保留
        git(&repo, &["config", "filter.x.clean", s]);
        fs::write(wt.join(".gitattributes"), "* filter=x\n").unwrap();
        assert!(matches!(status(&wt), Some(Status::Keep(r)) if r.contains("filter")));
        assert!(!marker.exists(), "clean filter must not run");
    }

    #[test]
    fn submodules_and_forged_admin_dirs_are_not_auto_checked() {
        let (_d, base) = base();
        let repo = repo(&base);
        let wt = add_worktree(&repo, &base, "sub");
        fs::write(wt.join(".gitmodules"), "").unwrap();
        assert!(matches!(status(&wt), Some(Status::Keep(r)) if r.contains("submodule")));

        // 管理目錄不在 <common>/worktrees/ 底下：不當成 worktree
        let fake = base.join("fake");
        let admin = base.join("admin");
        fs::create_dir_all(&fake).unwrap();
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("commondir"), repo.join(".git").to_str().unwrap()).unwrap();
        fs::write(admin.join("gitdir"), fake.join(".git").to_str().unwrap()).unwrap();
        fs::write(fake.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        assert_eq!(status(&fake), None);
    }

    #[test]
    fn missing_git_keeps_worktree() {
        let (_d, base) = base();
        let repo = repo(&base);
        let wt = add_worktree(&repo, &base, "feat");
        assert!(matches!(inspect(&wt, None), Some((_, Status::Keep(_)))));
    }

    #[test]
    fn prune_removes_metadata_of_deleted_worktree() {
        let (_d, base) = base();
        let repo = repo(&base);
        let wt = add_worktree(&repo, &base, "gone");
        let common = common_dir(&wt).unwrap();
        fs::remove_dir_all(&wt).unwrap();
        assert!(repo.join(".git/worktrees/gone").exists());
        prune(&git_bin(), &common).unwrap();
        assert!(!repo.join(".git/worktrees/gone").exists());
        // branch 保留
        git(&repo, &["rev-parse", "--verify", "-q", "refs/heads/gone"]);
    }
}
