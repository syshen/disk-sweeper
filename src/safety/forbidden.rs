//! 硬性禁區：寫死在程式裡，不可設定。比對一律不分 ASCII 大小寫（APFS 預設不分大小寫）。
//! 所有 rel 都是相對於 home、以 "/" 分隔、沒有開頭斜線的路徑；"" 代表 home 本身。

/// 本身不可刪除（內容物可依規則處理）
pub const PROTECTED_SELF: &[&str] = &[
    "",
    "Library",
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Music",
    "Movies",
    "Applications",
    "Library/Caches",
    "Library/Application Support",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Developer",
    "Library/Logs",
];

/// 整棵子樹不可刪除
pub const PROTECTED_TREE: &[&str] = &[
    "Library/Keychains",
    "Library/Mobile Documents",
    "Library/Mail",
    "Library/Messages",
    "Library/Accounts",
    "Library/Cookies",
    "Library/Preferences",
    "Library/CloudStorage",
    "Library/Logs/disk-sweeper",
    ".ssh",
    ".gnupg",
    ".config",
];

const REASON_SELF: &str = "系統或使用者的根目錄本身不可刪除";
const REASON_TREE: &str = "受保護的路徑（金鑰、iCloud、郵件、偏好設定等）";
const REASON_CONTAINS: &str = "此目錄包含受保護的路徑";
const REASON_GIT: &str = "git 儲存庫的內部資料";
const REASON_PHOTOS: &str = "照片圖庫";
const PHOTOS_SUFFIX: &str = ".photoslibrary";

/// rel 等於 base，或位於 base 之下（以路徑元件為單位）
fn within_ci(rel: &str, base: &str) -> bool {
    rel.len() >= base.len()
        && rel.is_char_boundary(base.len())
        && rel[..base.len()].eq_ignore_ascii_case(base)
        && (rel.len() == base.len() || rel.as_bytes()[base.len()] == b'/')
}

fn special_component(rel: &str) -> Option<&'static str> {
    for c in rel.split('/') {
        if c.eq_ignore_ascii_case(".git") {
            return Some(REASON_GIT);
        }
        let n = PHOTOS_SUFFIX.len();
        if c.len() >= n && c.is_char_boundary(c.len() - n) && c[c.len() - n..].eq_ignore_ascii_case(PHOTOS_SUFFIX) {
            return Some(REASON_PHOTOS);
        }
    }
    None
}

pub fn protection_reason_rel(rel: &str) -> Option<&'static str> {
    let rel = rel.trim_end_matches('/');
    if PROTECTED_SELF.iter().any(|p| p.eq_ignore_ascii_case(rel)) {
        return Some(REASON_SELF);
    }
    if PROTECTED_TREE.iter().any(|p| within_ci(rel, p)) {
        return Some(REASON_TREE);
    }
    if PROTECTED_TREE.iter().any(|p| within_ci(p, rel)) {
        return Some(REASON_CONTAINS);
    }
    special_component(rel)
}

/// 規則引擎用：是否整棵都不該被比對（不往下走）
pub fn is_protected_tree_rel(rel: &str) -> bool {
    PROTECTED_TREE.iter().any(|p| within_ci(rel, p)) || special_component(rel).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_is_component_aware_and_case_insensitive() {
        assert!(within_ci("Library/Keychains", "Library/Keychains"));
        assert!(within_ci("library/KEYCHAINS/login", "Library/Keychains"));
        assert!(!within_ci("Library/KeychainsBackup", "Library/Keychains"));
        assert!(!within_ci("Library", "Library/Keychains"));
    }

    #[test]
    fn classifies_relative_paths() {
        assert!(protection_reason_rel("").is_some());
        assert!(protection_reason_rel("Library/Caches").is_some());
        assert!(protection_reason_rel("Library/Caches/com.foo").is_none());
        assert!(protection_reason_rel(".ssh/id_ed25519").is_some());
        assert!(protection_reason_rel("Projects/repo/.git/objects").is_some());
        assert!(protection_reason_rel("Pictures/My.photoslibrary").is_some());
        assert!(protection_reason_rel("Projects/repo/src").is_none());
    }

    #[test]
    fn protected_tree_check_for_rules() {
        assert!(is_protected_tree_rel("Library/Mobile Documents/com~apple~CloudDocs"));
        assert!(is_protected_tree_rel("Library/Logs/disk-sweeper"));
        assert!(!is_protected_tree_rel("Library"));
        assert!(!is_protected_tree_rel(""));
    }
}
