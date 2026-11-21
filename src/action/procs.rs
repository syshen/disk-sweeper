use std::process::{Command, Stdio};

/// 直接刪檔的規則對應的 App：執行中會一邊寫入，建議先關閉再清理
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolApp {
    pub name: &'static str,
    pub process: &'static str,
}

/// (規則 id, App)。Docker / OrbStack 的目錄規則沒有可自動執行的指令、不會被核准，所以不在此列。
const APPS: &[(&str, ToolApp)] = &[
    ("xcode-derived-data", XCODE),
    ("xcode-archives", XCODE),
    ("xcode-device-support", XCODE),
    (
        "simulator",
        ToolApp {
            name: "Simulator",
            process: "Simulator",
        },
    ),
];

const XCODE: ToolApp = ToolApp {
    name: "Xcode",
    process: "Xcode",
};

/// 依顯示名稱去重。
pub fn apps_for_rules<'a>(rule_ids: impl IntoIterator<Item = &'a str>) -> Vec<ToolApp> {
    let mut out: Vec<ToolApp> = Vec::new();
    for id in rule_ids {
        for &(rule, app) in APPS {
            if rule == id && !out.iter().any(|a| a.name == app.name) {
                out.push(app);
            }
        }
    }
    out
}

pub fn is_running(process: &str) -> bool {
    Command::new("pgrep")
        .arg("-x")
        .arg(process)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn running_app_warnings<'a>(rule_ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    warnings_with(rule_ids, is_running)
}

fn warnings_with<'a>(
    rule_ids: impl IntoIterator<Item = &'a str>,
    running: impl Fn(&str) -> bool,
) -> Vec<String> {
    apps_for_rules(rule_ids)
        .into_iter()
        .filter(|app| running(app.process))
        .map(|app| format!("{} 正在執行，建議先關閉再清理", app.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_rules_to_apps_without_duplicates() {
        let apps = apps_for_rules([
            "xcode-derived-data",
            "xcode-archives",
            "simulator",
            "npm-cache",
        ]);
        let names: Vec<&str> = apps.iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["Xcode", "Simulator"]);
    }

    #[test]
    fn xcode_warns_only_while_running() {
        let ids = ["xcode-derived-data", "simulator"];
        assert_eq!(
            warnings_with(ids, |_| true),
            vec![
                "Xcode 正在執行，建議先關閉再清理",
                "Simulator 正在執行，建議先關閉再清理"
            ]
        );
        assert!(warnings_with(ids, |_| false).is_empty());
    }

    #[test]
    fn unknown_process_is_not_running() {
        assert!(!is_running("disk-sweeper-no-such-process-xyz"));
    }
}
