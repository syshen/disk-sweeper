/// 風險等級決定處理方式：可重建 → 直接刪除；使用者資料 → 垃圾桶；
/// 工具管理 → 執行官方指令；可移除的 git worktree → 再檢查後移到垃圾桶並 prune；
/// Docker 的 volume / container → 確認後用 docker 指令移除（無法復原）；受保護 → 不可處理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Risk {
    Regenerable,
    UserData,
    Protected,
    ToolManaged,
    /// 經 git 檢查可安全移除的 linked worktree
    Worktree,
    /// Docker 裡沒有被使用、但刪了救不回的資料（匿名 volume、已停止的 container）
    DockerData,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Regenerable => "可重建",
            Risk::UserData => "使用者資料",
            Risk::Protected => "受保護",
            Risk::ToolManaged => "工具管理",
            Risk::Worktree => "worktree",
            Risk::DockerData => "無法復原",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
}

impl ToolCommand {
    pub fn display(&self) -> String {
        std::iter::once(self.program)
            .chain(self.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    Delete,
    Trash,
    Command(ToolCommand),
    /// 動手前再用 git 檢查一次，移到垃圾桶後在主 repo 執行 `git worktree prune`
    RemoveWorktree,
    /// 動手前重查一次 Docker 狀態，再用 docker 指令逐個移除
    Docker,
}

impl Handling {
    pub fn label(&self) -> &'static str {
        match self {
            Handling::Delete => "直接刪除",
            Handling::Trash => "移到垃圾桶",
            Handling::Command(_) => "執行指令",
            Handling::RemoveWorktree => "移除 worktree",
            Handling::Docker => "Docker 移除",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_display_joins_args() {
        let cmd = ToolCommand {
            program: "brew",
            args: &["cleanup", "--prune=all"],
        };
        assert_eq!(cmd.display(), "brew cleanup --prune=all");
    }
}
