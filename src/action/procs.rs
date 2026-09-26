use std::process::{Command, Stdio};

/// (規則 id, 顯示名稱, 程序名稱)
const APPS: &[(&str, &str, &str)] = &[
    ("xcode-derived-data", "Xcode", "Xcode"),
    ("xcode-archives", "Xcode", "Xcode"),
    ("xcode-device-support", "Xcode", "Xcode"),
    ("simulator", "Simulator", "Simulator"),
    ("docker", "Docker Desktop", "com.docker.backend"),
    ("orbstack", "OrbStack", "OrbStack"),
];

/// 回傳 (顯示名稱, 程序名稱)，依顯示名稱去重。
pub fn apps_for_rules<'a>(
    rule_ids: impl IntoIterator<Item = &'a str>,
) -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&'static str, &'static str)> = Vec::new();
    for id in rule_ids {
        for &(rule, name, process) in APPS {
            if rule == id && !out.iter().any(|(n, _)| *n == name) {
                out.push((name, process));
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
    apps_for_rules(rule_ids)
        .into_iter()
        .filter(|(_, process)| is_running(process))
        .map(|(name, _)| format!("{name} 正在執行，建議先關閉再清理"))
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
        assert_eq!(
            apps,
            vec![("Xcode", "Xcode"), ("Docker Desktop", "com.docker.backend")]
        );
    }

    #[test]
    fn unknown_process_is_not_running() {
        assert!(!is_running("disk-sweeper-no-such-process-xyz"));
    }
}
