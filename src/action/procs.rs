use std::process::{Command, Stdio};

/// 清理時這個 App 應處於的狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// 直接刪檔的規則：App 執行中會一邊寫入，建議先關閉
    Quit,
    /// 用官方指令清理的規則：App 必須正在執行，指令才會成功
    Running { command: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolApp {
    pub name: &'static str,
    pub process: &'static str,
    pub expect: Expect,
}

/// (規則 id, App)。OrbStack 規則沒有可自動執行的指令、不會被核准，所以不在此列。
const APPS: &[(&str, ToolApp)] = &[
    ("xcode-derived-data", XCODE),
    ("xcode-archives", XCODE),
    ("xcode-device-support", XCODE),
    (
        "simulator",
        ToolApp {
            name: "Simulator",
            process: "Simulator",
            expect: Expect::Quit,
        },
    ),
    (
        "docker",
        ToolApp {
            name: "Docker Desktop",
            process: "com.docker.backend",
            expect: Expect::Running { command: "docker" },
        },
    ),
];

const XCODE: ToolApp = ToolApp {
    name: "Xcode",
    process: "Xcode",
    expect: Expect::Quit,
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
        .filter_map(|app| match (app.expect, running(app.process)) {
            (Expect::Quit, true) => Some(format!("{} 正在執行，建議先關閉再清理", app.name)),
            (Expect::Running { command }, false) => {
                Some(format!("{} 未執行，{command} 指令會失敗", app.name))
            }
            _ => None,
        })
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
            "docker",
            "npm-cache",
        ]);
        let names: Vec<&str> = apps.iter().map(|a| a.name).collect();
        assert_eq!(names, vec!["Xcode", "Docker Desktop"]);
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
    fn docker_warns_only_when_not_running() {
        assert_eq!(
            warnings_with(["docker"], |_| false),
            vec!["Docker Desktop 未執行，docker 指令會失敗"]
        );
        assert!(warnings_with(["docker"], |_| true).is_empty());
    }

    #[test]
    fn unknown_process_is_not_running() {
        assert!(!is_running("disk-sweeper-no-such-process-xyz"));
    }
}
