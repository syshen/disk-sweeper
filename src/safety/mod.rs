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
    /// `Handling::Command` 時，審核當下在 PATH 找到的指令絕對路徑
    program_path: Option<PathBuf>,
    rule_id: Option<&'static str>,
    size: u64,
    dev: u64,
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
    /// 命令類項目要執行的程式（絕對路徑）；其他處理方式為 `None`
    pub fn program_path(&self) -> Option<&Path> {
        self.program_path.as_deref()
    }
    pub fn rule_id(&self) -> Option<&'static str> {
        self.rule_id
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    /// 審核時 lstat 取得的裝置編號（必定與 home 相同）
    pub fn dev(&self) -> u64 {
        self.dev
    }
    pub fn inode(&self) -> u64 {
        self.inode
    }
    pub fn mtime(&self) -> i64 {
        self.mtime
    }

    #[cfg(test)]
    pub(crate) fn set_dev_for_test(&mut self, dev: u64) {
        self.dev = dev;
    }
}

#[derive(Debug)]
pub enum Verdict {
    Approved(ApprovedTarget),
    Rejected { path: PathBuf, reason: String },
}

pub fn review(c: Candidate, ctx: &Context) -> Verdict {
    match check(&c, ctx) {
        Ok(ok) => Verdict::Approved(ApprovedTarget {
            path: ok.path,
            handling: ok.handling,
            program_path: ok.program_path,
            rule_id: c.rule_id,
            size: c.size,
            dev: ok.dev,
            inode: c.inode,
            mtime: c.mtime,
        }),
        Err(reason) => Verdict::Rejected {
            path: c.path,
            reason,
        },
    }
}

struct Checked {
    path: PathBuf,
    handling: Handling,
    program_path: Option<PathBuf>,
    dev: u64,
}

/// 目標必須和 home 在同一個磁碟區（st_dev 相同）：掛載點與其內容一律不處理。
fn same_volume(target_dev: u64, home_dev: u64) -> bool {
    target_dev == home_dev
}

fn check(c: &Candidate, ctx: &Context) -> Result<Checked, String> {
    let (resolved, meta) = resolve(&c.path)?;
    let rel = resolved
        .strip_prefix(&ctx.home)
        .map_err(|_| "不在家目錄之下".to_string())?;
    let rel = rel.to_str().ok_or("路徑不是有效的 UTF-8")?;
    if let Some(reason) = forbidden::protection_reason_rel(rel) {
        return Err(reason.into());
    }
    if !same_volume(meta.dev(), ctx.home_dev) {
        return Err("位於其他磁碟區，不處理".into());
    }
    let mut program_path = None;
    let handling = match c.risk {
        Risk::Protected => return Err("受保護的項目".into()),
        // 只有比對到規則的「可重建」才直接刪除；其餘一律最多只能丟垃圾桶
        Risk::Regenerable if c.rule_id.is_some() => Handling::Delete,
        Risk::Regenerable | Risk::UserData => Handling::Trash,
        Risk::ToolManaged => {
            let cmd = c
                .command
                .ok_or("此項目沒有可自動執行的官方指令，請手動處理")?;
            program_path = Some(
                find_in_path(cmd.program, &ctx.path_var)
                    .ok_or_else(|| format!("找不到指令 `{}`", cmd.program))?,
            );
            Handling::Command(cmd)
        }
    };
    // SystemTrash 一律拒絕符號連結；在這裡就拒絕，不要核准一個執行時必定失敗的項目
    if handling == Handling::Trash && meta.file_type().is_symlink() {
        return Err("符號連結無法移到垃圾桶".into());
    }
    Ok(Checked {
        path: resolved,
        handling,
        program_path,
        dev: meta.dev(),
    })
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
///
/// 同時回傳目標本身的 lstat 結果（不跟隨 symlink）。
fn resolve(path: &Path) -> Result<(PathBuf, fs::Metadata), String> {
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
        let real = joined
            .canonicalize()
            .map_err(|e| format!("無法解析路徑：{e}"))?;
        return Ok((real, meta));
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
                return Ok((parent.join(entry.file_name()), meta));
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
        // 連結本身若會被丟垃圾桶 → 拒絕（SystemTrash 一律拒絕符號連結，核准只會在執行時失敗）
        match review(cand(link.clone(), Risk::UserData, None), &ctx) {
            Verdict::Rejected { reason, .. } => assert_eq!(reason, "符號連結無法移到垃圾桶"),
            Verdict::Approved(t) => panic!("approved {:?}", t.path()),
        }
        // 可重建且比對到規則 → 直接刪除（只移除連結本身）
        match review(cand(link.clone(), Risk::Regenerable, Some("r")), &ctx) {
            Verdict::Approved(t) => {
                assert_eq!(t.path(), link.as_path());
                assert_eq!(t.handling(), Handling::Delete);
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
        match review(cand(folded, Risk::Regenerable, Some("r")), &ctx) {
            Verdict::Approved(t) => {
                // 一定要解析成連結自己在磁碟上的真實拼寫，不可用呼叫端傳入的折疊拼寫。
                assert_eq!(t.path(), link.as_path());
                assert_eq!(t.handling(), Handling::Delete);
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
        match review(ok, &ctx) {
            Verdict::Approved(t) => {
                assert_eq!(t.handling(), Handling::Command(cmd));
                // 執行時要跑的就是這裡檢查過的絕對路徑，不再重新查 PATH
                let program = t.program_path().expect("command target has a program path");
                assert!(program.is_absolute());
                assert_eq!(program, Path::new(&ctx.path_var).join("fakebrew"));
            }
            Verdict::Rejected { reason, .. } => panic!("{reason}"),
        }
    }

    #[test]
    fn non_command_targets_have_no_program_path() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Documents/report.pdf");
        match review(cand(p, Risk::UserData, None), &ctx) {
            Verdict::Approved(t) => assert!(t.program_path().is_none()),
            Verdict::Rejected { reason, .. } => panic!("{reason}"),
        }
    }

    #[test]
    fn same_volume_compares_devices() {
        assert!(same_volume(7, 7));
        assert!(!same_volume(7, 8));
    }

    #[test]
    fn target_on_other_volume_is_rejected() {
        let (_d, mut ctx) = setup();
        let p = ctx.home.join("Documents/report.pdf");
        let dev = fs::symlink_metadata(&p).unwrap().dev();
        match review(cand(p.clone(), Risk::UserData, None), &ctx) {
            Verdict::Approved(t) => assert_eq!(t.dev(), dev),
            Verdict::Rejected { reason, .. } => panic!("{reason}"),
        }
        // 假裝 home 在另一個磁碟區上
        ctx.home_dev = dev.wrapping_add(1);
        match review(cand(p, Risk::UserData, None), &ctx) {
            Verdict::Rejected { reason, .. } => assert_eq!(reason, "位於其他磁碟區，不處理"),
            Verdict::Approved(t) => panic!("approved {:?}", t.path()),
        }
    }

    /// 真的掛載一個磁碟映像到假 home 底下，確認掛載點會被拒絕。
    /// 需要 hdiutil（macOS），執行：cargo test -- --ignored mounted_volume
    #[test]
    #[ignore]
    fn mounted_volume_under_home_is_rejected() {
        use std::process::Command;

        struct Detach(PathBuf);
        impl Drop for Detach {
            fn drop(&mut self) {
                let _ = Command::new("hdiutil")
                    .args(["detach", "-force"])
                    .arg(&self.0)
                    .output();
            }
        }

        let (d, ctx) = setup();
        let img = d.path().join("vol.dmg");
        let ok = Command::new("hdiutil")
            .args([
                "create",
                "-size",
                "2m",
                "-fs",
                "HFS+",
                "-volname",
                "dsweep-test",
            ])
            .arg(&img)
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stderr)
        );
        let mnt = ctx.home.join("Projects/mnt");
        fs::create_dir_all(&mnt).unwrap();
        let ok = Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-mountpoint"])
            .arg(&mnt)
            .arg(&img)
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stderr)
        );
        let _guard = Detach(mnt.clone());
        fs::write(mnt.join("f.bin"), b"x").unwrap();

        for p in [mnt.clone(), mnt.join("f.bin")] {
            match review(cand(p.clone(), Risk::Regenerable, Some("r")), &ctx) {
                Verdict::Rejected { reason, .. } => {
                    assert_eq!(reason, "位於其他磁碟區，不處理", "{p:?}")
                }
                Verdict::Approved(t) => panic!("approved {:?}", t.path()),
            }
        }
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
            prop_assume!(name != "." && name != "..");
            let (_d, ctx) = setup();
            // 真的在磁碟上建立檔案，讓測試走過 resolve()，而不只是「路徑不存在」的拒絕
            let p = ctx.home.join(base).join(&name);
            fs::write(&p, b"x").unwrap();
            match review(cand(p, risk, Some("r")), &ctx) {
                Verdict::Rejected { reason, .. } => prop_assert_ne!(reason, "路徑不存在"),
                Verdict::Approved(t) => prop_assert!(false, "approved {:?}", t.path()),
            }
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
