//! 安全守門員：所有檔案系統變更的唯一關卡。
//! `ApprovedTarget` 的欄位私有，只能由本模組的 `review` 建立，
//! 而 `action::execute` 只接受 `ApprovedTarget`，所以從型別上無法繞過檢查。

pub mod forbidden;

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use crate::context::Context;
use crate::risk::{Handling, Risk, ToolCommand};

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub risk: Risk,
    pub rule_id: Option<&'static str>,
    pub command: Option<ToolCommand>,
    pub size: u64,
    pub inode: u64,
    pub mtime: i64,
}

#[derive(Debug)]
pub struct ApprovedTarget {
    path: PathBuf,
    handling: Handling,
    rule_id: Option<&'static str>,
    size: u64,
    inode: u64,
    mtime: i64,
}

impl ApprovedTarget {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn handling(&self) -> Handling {
        self.handling
    }
    pub fn rule_id(&self) -> Option<&'static str> {
        self.rule_id
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn inode(&self) -> u64 {
        self.inode
    }
    pub fn mtime(&self) -> i64 {
        self.mtime
    }
}

#[derive(Debug)]
pub enum Verdict {
    Approved(ApprovedTarget),
    Rejected { path: PathBuf, reason: String },
}

pub fn review(c: Candidate, ctx: &Context) -> Verdict {
    match check(&c, ctx) {
        Ok((path, handling)) => Verdict::Approved(ApprovedTarget {
            path,
            handling,
            rule_id: c.rule_id,
            size: c.size,
            inode: c.inode,
            mtime: c.mtime,
        }),
        Err(reason) => Verdict::Rejected {
            path: c.path,
            reason,
        },
    }
}

fn check(c: &Candidate, ctx: &Context) -> Result<(PathBuf, Handling), String> {
    let resolved = resolve(&c.path)?;
    let rel = resolved
        .strip_prefix(&ctx.home)
        .map_err(|_| "不在家目錄之下".to_string())?;
    let rel = rel.to_str().ok_or("路徑不是有效的 UTF-8")?;
    if let Some(reason) = forbidden::protection_reason_rel(rel) {
        return Err(reason.into());
    }
    let handling = match c.risk {
        Risk::Protected => return Err("受保護的項目".into()),
        // 只有比對到規則的「可重建」才直接刪除；其餘一律最多只能丟垃圾桶
        Risk::Regenerable if c.rule_id.is_some() => Handling::Delete,
        Risk::Regenerable | Risk::UserData => Handling::Trash,
        Risk::ToolManaged => {
            let cmd = c
                .command
                .ok_or("此項目沒有可自動執行的官方指令，請手動處理")?;
            if find_in_path(cmd.program, &ctx.path_var).is_none() {
                return Err(format!("找不到指令 `{}`", cmd.program));
            }
            Handling::Command(cmd)
        }
    };
    Ok((resolved, handling))
}

/// 解析上層目錄的 symlink（目標本身不跟隨），拒絕含 `..` 的路徑，
/// 並把最後一個路徑元件解析成磁碟上實際的拼寫。
///
/// APFS 預設（大小寫不敏感）在查找檔名時，除了忽略大小寫，還會對某些
/// Unicode 字元做折疊比對 —— 例如 U+212A KELVIN SIGN 會被當成 `K`、
/// U+017F LATIN SMALL LETTER LONG S 會被當成 `s`。若後續的保護清單比對
/// 只用 ASCII case-insensitive 字串比對呼叫端傳入的原始拼寫，就可能被
/// 這類折疊字元繞過。因此這裡一律先用檔案系統查出真正的拼寫，再交給
/// `forbidden` 模組做字串比對。
fn resolve(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("必須是絕對路徑".into());
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("路徑不可包含 `..`".into());
    }
    let name = path.file_name().ok_or("路徑沒有檔名")?;
    let parent = path.parent().ok_or("路徑沒有上層目錄")?;
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("無法解析上層目錄：{e}"))?;
    let joined = parent.join(name);

    let meta = match fs::symlink_metadata(&joined) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err("路徑不存在".into()),
        Err(e) => return Err(e.to_string()),
    };

    if !meta.file_type().is_symlink() {
        // 不是 symlink：直接 canonicalize 取得磁碟上的真實拼寫。因為最後一個
        // 元件本身確定不是 symlink，這不會意外跟隨到其他檔案。
        return joined
            .canonicalize()
            .map_err(|e| format!("無法解析路徑：{e}"));
    }

    // 是 symlink：目標本身不可跟隨，改為在上層目錄裡找出它自己的真實名稱
    // （以 (dev, inode) 比對，因為呼叫端拼寫可能是折疊變體）。
    let target_dev = meta.dev();
    let target_ino = meta.ino();
    let entries = fs::read_dir(&parent).map_err(|e| format!("無法讀取上層目錄：{e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("無法讀取上層目錄：{e}"))?;
        if let Ok(m) = entry.metadata() {
            if m.dev() == target_dev && m.ino() == target_ino {
                return Ok(parent.join(entry.file_name()));
            }
        }
    }
    Err("找不到符號連結的實際名稱".into())
}

/// 純字串比對（不碰檔案系統），給 UI 標示「受保護」用。
pub fn protection_reason(path: &Path, ctx: &Context) -> Option<&'static str> {
    match path.strip_prefix(&ctx.home) {
        Ok(rel) => forbidden::protection_reason_rel(&rel.to_string_lossy()),
        Err(_) => Some("不在家目錄之下"),
    }
}

pub fn find_in_path(program: &str, path_var: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_var)
        .filter(|dir| !dir.as_os_str().is_empty() && dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Context) {
        let d = tempfile::tempdir().unwrap();
        for p in [
            "Library/Keychains",
            "Library/Caches/com.example",
            "Library/Mail",
            ".ssh",
            ".config",
            "Documents",
            "Desktop",
            "Projects/repo/.git",
            "Pictures/Photos Library.photoslibrary",
            "Pictures/P.photoslibrary",
            "bin",
        ] {
            fs::create_dir_all(d.path().join(p)).unwrap();
        }
        fs::write(d.path().join("Documents/report.pdf"), b"pdf").unwrap();
        let bin = d.path().join("bin/fakebrew");
        fs::write(&bin, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let ctx = Context::new(d.path(), d.path().join("bin").into_os_string()).unwrap();
        (d, ctx)
    }

    fn cand(path: PathBuf, risk: Risk, rule: Option<&'static str>) -> Candidate {
        Candidate {
            path,
            risk,
            rule_id: rule,
            command: None,
            size: 1,
            inode: 1,
            mtime: 0,
        }
    }

    fn rejected(v: &Verdict) -> bool {
        matches!(v, Verdict::Rejected { .. })
    }

    fn handling(v: Verdict) -> Handling {
        match v {
            Verdict::Approved(t) => t.handling(),
            Verdict::Rejected { reason, .. } => panic!("rejected: {reason}"),
        }
    }

    #[test]
    fn home_and_root_dirs_are_rejected() {
        let (_d, ctx) = setup();
        for rel in ["", "Library", "Documents", "Library/Caches"] {
            let p = if rel.is_empty() {
                ctx.home.clone()
            } else {
                ctx.home.join(rel)
            };
            assert!(
                rejected(&review(cand(p, Risk::Regenerable, Some("r")), &ctx)),
                "{rel}"
            );
        }
    }

    #[test]
    fn regenerable_with_rule_is_deleted() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Library/Caches/com.example");
        assert_eq!(
            handling(review(cand(p, Risk::Regenerable, Some("app-caches")), &ctx)),
            Handling::Delete
        );
    }

    #[test]
    fn regenerable_without_rule_is_downgraded_to_trash() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Library/Caches/com.example");
        assert_eq!(
            handling(review(cand(p, Risk::Regenerable, None), &ctx)),
            Handling::Trash
        );
    }

    #[test]
    fn user_data_goes_to_trash() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Documents/report.pdf");
        assert_eq!(
            handling(review(cand(p, Risk::UserData, None), &ctx)),
            Handling::Trash
        );
    }

    #[test]
    fn protected_risk_is_rejected() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Documents/report.pdf");
        assert!(rejected(&review(cand(p, Risk::Protected, None), &ctx)));
    }

    #[test]
    fn protected_trees_are_rejected() {
        let (_d, ctx) = setup();
        for rel in [
            "Library/Keychains/login.keychain-db",
            ".ssh/id_ed25519",
            "Library/Mail/V10",
            ".config/fish",
        ] {
            assert!(
                rejected(&review(
                    cand(ctx.home.join(rel), Risk::UserData, None),
                    &ctx
                )),
                "{rel}"
            );
        }
    }

    #[test]
    fn case_variants_of_protected_paths_are_rejected() {
        let (_d, ctx) = setup();
        for rel in [
            "library/KEYCHAINS/x",
            ".SSH/id",
            "LIBRARY",
            "library/caches",
        ] {
            assert!(
                rejected(&review(
                    cand(ctx.home.join(rel), Risk::Regenerable, Some("r")),
                    &ctx
                )),
                "{rel}"
            );
        }
    }

    #[test]
    fn outside_home_relative_and_dotdot_paths_are_rejected() {
        let (_d, ctx) = setup();
        for p in [
            PathBuf::from("/etc/hosts"),
            PathBuf::from("relative/x"),
            ctx.home.join("Projects/../.ssh/id"),
        ] {
            assert!(
                rejected(&review(cand(p.clone(), Risk::UserData, None), &ctx)),
                "{p:?}"
            );
        }
    }

    #[test]
    fn symlink_into_protected_dir_is_rejected_but_link_itself_is_allowed() {
        let (_d, ctx) = setup();
        let link = ctx.home.join("sneaky");
        std::os::unix::fs::symlink(ctx.home.join(".ssh"), &link).unwrap();
        assert!(rejected(&review(
            cand(link.join("id_rsa"), Risk::UserData, None),
            &ctx
        )));
        match review(cand(link.clone(), Risk::UserData, None), &ctx) {
            Verdict::Approved(t) => {
                assert_eq!(t.path(), link.as_path());
                assert_eq!(t.handling(), Handling::Trash);
            }
            Verdict::Rejected { reason, .. } => panic!("{reason}"),
        }
    }

    #[test]
    fn unicode_fold_variants_of_protected_paths_are_rejected() {
        let (_d, ctx) = setup();
        for rel in [
            "Library/\u{212A}eychains",
            "Library/\u{212A}eychains/",
            ".\u{017F}sh",
            "Des\u{212A}top",
            "Pictures/P.photo\u{017F}library",
        ] {
            assert!(
                rejected(&review(
                    cand(ctx.home.join(rel), Risk::Regenerable, Some("r")),
                    &ctx
                )),
                "{rel}"
            );
        }
    }

    #[test]
    fn symlink_fold_variant_name_resolves_to_real_name_or_is_rejected() {
        let (_d, ctx) = setup();
        let link = ctx.home.join("sneaky");
        std::os::unix::fs::symlink(ctx.home.join(".ssh"), &link).unwrap();
        let folded = ctx.home.join("\u{017F}neaky");
        match review(cand(folded, Risk::UserData, None), &ctx) {
            Verdict::Approved(t) => {
                // 一定要解析成連結自己在磁碟上的真實拼寫，不可用呼叫端傳入的折疊拼寫。
                assert_eq!(t.path(), link.as_path());
                assert_eq!(t.handling(), Handling::Trash);
            }
            Verdict::Rejected { .. } => {
                // 若這個檔案系統的折疊查找找不到它，拒絕也是可接受的結果 ——
                // 唯一不可接受的是「核准了一個拼寫不同的路徑」。
            }
        }
    }

    #[test]
    fn find_in_path_skips_empty_and_relative_entries() {
        let (_d, ctx) = setup();
        let real_bin = ctx.home.join("bin");
        let path_var = std::ffi::OsString::from(format!(":relative/bin:{}", real_bin.display()));
        assert_eq!(
            find_in_path("fakebrew", &path_var),
            Some(real_bin.join("fakebrew"))
        );
    }

    #[test]
    fn git_internals_and_photos_library_are_rejected() {
        let (_d, ctx) = setup();
        for rel in [
            "Projects/repo/.git",
            "Projects/repo/.git/objects",
            "Pictures/Photos Library.photoslibrary",
        ] {
            assert!(
                rejected(&review(
                    cand(ctx.home.join(rel), Risk::UserData, None),
                    &ctx
                )),
                "{rel}"
            );
        }
    }

    #[test]
    fn tool_managed_requires_available_command() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Library/Caches/com.example");
        assert!(rejected(&review(
            cand(p.clone(), Risk::ToolManaged, Some("t")),
            &ctx
        )));
        let mut missing = cand(p.clone(), Risk::ToolManaged, Some("t"));
        missing.command = Some(ToolCommand {
            program: "nope-not-here",
            args: &[],
        });
        assert!(rejected(&review(missing, &ctx)));
        let mut ok = cand(p, Risk::ToolManaged, Some("t"));
        let cmd = ToolCommand {
            program: "fakebrew",
            args: &["cleanup"],
        };
        ok.command = Some(cmd);
        assert_eq!(handling(review(ok, &ctx)), Handling::Command(cmd));
    }

    #[test]
    fn protection_reason_is_lexical_and_case_insensitive() {
        let (_d, ctx) = setup();
        assert!(protection_reason(&ctx.home.join("library/keychains"), &ctx).is_some());
        assert!(protection_reason(&ctx.home.join("Projects/app"), &ctx).is_none());
        assert!(protection_reason(Path::new("/etc"), &ctx).is_some());
    }

    proptest! {
        #[test]
        fn anything_directly_under_protected_trees_is_rejected(
            base in prop::sample::select(vec!["Library/Keychains", ".ssh", "Library/Mail", ".config"]),
            name in "[a-zA-Z0-9 ._中文\u{212A}\u{017F}-]{1,20}",
            risk in prop::sample::select(vec![Risk::Regenerable, Risk::UserData, Risk::ToolManaged]),
        ) {
            let (_d, ctx) = setup();
            let v = review(cand(ctx.home.join(base).join(&name), risk, Some("r")), &ctx);
            prop_assert!(rejected(&v));
        }

        #[test]
        fn paths_with_parent_components_are_always_rejected(
            segs in prop::collection::vec(prop::sample::select(vec!["a", "..", ".", "Library", "Caches"]), 1..6),
        ) {
            let (_d, ctx) = setup();
            let mut p = ctx.home.clone();
            for s in &segs {
                p.push(s);
            }
            let v = review(cand(p, Risk::Regenerable, Some("r")), &ctx);
            if segs.contains(&"..") {
                prop_assert!(rejected(&v));
            }
        }
    }
}
