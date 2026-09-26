/// 風險等級決定處理方式：可重建 → 直接刪除；使用者資料 → 垃圾桶；
/// 工具管理 → 執行官方指令；受保護 → 不可處理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Risk {
    Regenerable,
    UserData,
    Protected,
    ToolManaged,
}

impl Risk {
    pub fn label(self) -> &'static str {
        match self {
            Risk::Regenerable => "可重建",
            Risk::UserData => "使用者資料",
            Risk::Protected => "受保護",
            Risk::ToolManaged => "工具管理",
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
        std::iter::once(self.program).chain(self.args.iter().copied()).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    Delete,
    Trash,
    Command(ToolCommand),
}

impl Handling {
    pub fn label(&self) -> &'static str {
        match self {
            Handling::Delete => "直接刪除",
            Handling::Trash => "移到垃圾桶",
            Handling::Command(_) => "執行指令",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_display_joins_args() {
        let cmd = ToolCommand { program: "brew", args: &["cleanup", "--prune=all"] };
        assert_eq!(cmd.display(), "brew cleanup --prune=all");
    }
}
