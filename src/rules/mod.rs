mod catalog;

pub use catalog::CATALOG;

use crate::context::Context;
use crate::risk::{Risk, ToolCommand};
use crate::safety::forbidden;
use crate::tree::{NodeId, NodeKind, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    DevTools,
    AppCache,
    LargeFiles,
}

impl Category {
    pub const ALL: [Category; 3] = [Category::DevTools, Category::AppCache, Category::LargeFiles];

    pub fn label(self) -> &'static str {
        match self {
            Category::DevTools => "開發工具",
            Category::AppCache => "系統與 App cache",
            Category::LargeFiles => "大型使用者檔案",
        }
    }
}

/// 路徑皆相對於 home、以 "/" 分隔。
#[derive(Debug, Clone, Copy)]
pub enum Matcher {
    /// 路徑完全相同
    Exact(&'static [&'static str]),
    /// 某些目錄的直接子項目
    ChildOf {
        parents: &'static [&'static str],
        exclude: &'static [&'static str],
    },
    /// 某目錄下、名稱以 suffix 結尾的直接子項目
    ChildWithSuffix {
        parent: &'static str,
        suffix: &'static str,
    },
    /// 名為 name 的目錄，同層有 sibling；路徑中不可有 `.` 開頭的目錄、`Library` 或另一個 name
    DirWithSibling {
        name: &'static str,
        sibling: &'static str,
    },
    /// under 之下任何深度、副檔名符合的檔案
    ExtUnder {
        under: &'static str,
        exts: &'static [&'static str],
    },
    /// parent 的直接子項目，超過指定天數未修改且夠大
    OldChild {
        parent: &'static str,
        min_age_days: i64,
        min_size: u64,
    },
    /// 任何夠大的一般檔案
    LargeFile { min_size: u64 },
}

#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub id: &'static str,
    pub category: Category,
    pub risk: Risk,
    pub matcher: Matcher,
    pub command: Option<ToolCommand>,
    pub description: &'static str,
}

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub node: NodeId,
    pub rule_id: &'static str,
    pub category: Category,
    pub risk: Risk,
    pub description: &'static str,
    pub command: Option<ToolCommand>,
    pub size: u64,
}

impl Matcher {
    fn matches(&self, tree: &Tree, id: NodeId, rel: &str, now: i64) -> bool {
        let node = tree.node(id);
        let (parent_rel, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        match *self {
            Matcher::Exact(paths) => paths.contains(&rel),
            Matcher::ChildOf { parents, exclude } => {
                parents.contains(&parent_rel) && !exclude.contains(&name)
            }
            Matcher::ChildWithSuffix { parent, suffix } => {
                parent_rel == parent && name.ends_with(suffix)
            }
            Matcher::DirWithSibling {
                name: want,
                sibling,
            } => {
                node.kind == NodeKind::Dir
                    && name == want
                    && !parent_rel
                        .split('/')
                        .any(|c| c == want || c.starts_with('.') || c == "Library")
                    && node
                        .parent
                        .and_then(|p| tree.child_named(p, sibling))
                        .is_some()
            }
            Matcher::ExtUnder { under, exts } => {
                node.kind == NodeKind::File
                    && rel.starts_with(under)
                    && rel.as_bytes().get(under.len()) == Some(&b'/')
                    && name
                        .rsplit_once('.')
                        .is_some_and(|(_, ext)| exts.iter().any(|e| e.eq_ignore_ascii_case(ext)))
            }
            Matcher::OldChild {
                parent,
                min_age_days,
                min_size,
            } => {
                parent_rel == parent
                    && node.size >= min_size
                    && now - node.mtime > min_age_days * 86_400
            }
            Matcher::LargeFile { min_size } => node.kind == NodeKind::File && node.size >= min_size,
        }
    }
}

/// 單次走訪樹：每個節點依 CATALOG 順序比對，第一個符合者勝出，且不再往下比對。
pub fn evaluate(tree: &Tree, ctx: &Context, now: i64) -> Vec<Suggestion> {
    let Ok(root_rel) = tree.root_path().strip_prefix(&ctx.home) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![(tree.root(), root_rel.to_string_lossy().into_owned())];
    while let Some((id, rel)) = stack.pop() {
        if forbidden::is_protected_tree_rel(&rel) {
            continue;
        }
        if id != tree.root() {
            if let Some(rule) = CATALOG
                .iter()
                .find(|r| r.matcher.matches(tree, id, &rel, now))
            {
                out.push(Suggestion {
                    node: id,
                    rule_id: rule.id,
                    category: rule.category,
                    risk: rule.risk,
                    description: rule.description,
                    command: rule.command,
                    size: tree.node(id).size,
                });
                continue;
            }
        }
        for &child in &tree.node(id).children {
            let name = &tree.node(child).name;
            let child_rel = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{rel}/{name}")
            };
            stack.push((child, child_rel));
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.size));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::testutil::{find, tree_from};
    use std::ffi::OsString;
    use std::path::PathBuf;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn ctx() -> Context {
        Context {
            home: PathBuf::from("/h"),
            home_dev: 0,
            path_var: OsString::new(),
        }
    }

    fn eval(t: &Tree) -> Vec<Suggestion> {
        evaluate(t, &ctx(), NOW)
    }

    fn found(t: &Tree, s: &[Suggestion]) -> Vec<(String, &'static str)> {
        let mut v: Vec<(String, &'static str)> = s
            .iter()
            .map(|x| {
                (
                    t.path_of(x.node)
                        .strip_prefix("/h")
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    x.rule_id,
                )
            })
            .collect();
        v.sort();
        v
    }

    fn pairs(items: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
        let mut v: Vec<(String, &'static str)> =
            items.iter().map(|(p, r)| (p.to_string(), *r)).collect();
        v.sort();
        v
    }

    #[test]
    fn xcode_derived_data_children_are_matched_not_descended() {
        let t = tree_from(
            "/h",
            &[("Library/Developer/Xcode/DerivedData/App-abc/Build/x", 10)],
        );
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[(
                "Library/Developer/Xcode/DerivedData/App-abc",
                "xcode-derived-data"
            )])
        );
    }

    #[test]
    fn node_modules_next_to_package_json_is_matched_once() {
        let t = tree_from(
            "/h",
            &[
                ("Projects/web/package.json", 1),
                ("Projects/web/node_modules/react/package.json", 1),
                ("Projects/web/node_modules/react/node_modules/x/i.js", 1),
            ],
        );
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[("Projects/web/node_modules", "node-modules")])
        );
    }

    #[test]
    fn node_modules_without_package_json_is_ignored() {
        let t = tree_from("/h", &[("Projects/web/node_modules/a/i.js", 1)]);
        assert!(eval(&t).is_empty());
    }

    #[test]
    fn node_modules_under_hidden_or_library_dirs_are_ignored() {
        let t = tree_from(
            "/h",
            &[
                (
                    ".nvm/versions/node/v24/lib/node_modules/pnpm/package.json",
                    1,
                ),
                (
                    ".nvm/versions/node/v24/lib/node_modules/pnpm/node_modules/a/i.js",
                    1,
                ),
                (".vscode/extensions/ext/package.json", 1),
                (".vscode/extensions/ext/node_modules/a/i.js", 1),
                ("Library/Application Support/Code/x/package.json", 1),
                ("Library/Application Support/Code/x/node_modules/a/i.js", 1),
            ],
        );
        assert!(eval(&t).iter().all(|s| s.rule_id != "node-modules"));
    }

    #[test]
    fn rust_target_next_to_cargo_toml_is_matched() {
        let t = tree_from(
            "/h",
            &[
                ("Projects/cli/Cargo.toml", 1),
                ("Projects/cli/target/debug/cli", 100),
            ],
        );
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[("Projects/cli/target", "rust-target")])
        );
    }

    #[test]
    fn specific_cache_rules_win_over_app_caches() {
        let t = tree_from(
            "/h",
            &[
                ("Library/Caches/Homebrew/x", 1),
                ("Library/Caches/Yarn/x", 1),
                ("Library/Caches/com.spotify.client/x", 1),
            ],
        );
        let s = eval(&t);
        assert_eq!(
            found(&t, &s),
            pairs(&[
                ("Library/Caches/Homebrew", "homebrew-cache"),
                ("Library/Caches/Yarn", "yarn-cache"),
                ("Library/Caches/com.spotify.client", "app-caches"),
            ])
        );
        let brew = s.iter().find(|x| x.rule_id == "homebrew-cache").unwrap();
        assert_eq!(brew.risk, Risk::ToolManaged);
        assert_eq!(brew.command.unwrap().program, "brew");
    }

    #[test]
    fn user_logs_skip_own_log_dir() {
        let t = tree_from(
            "/h",
            &[
                ("Library/Logs/DiagnosticReports/a", 1),
                ("Library/Logs/disk-sweeper/actions.jsonl", 1),
            ],
        );
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[("Library/Logs/DiagnosticReports", "user-logs")])
        );
    }

    #[test]
    fn installers_win_over_old_downloads() {
        let mut t = tree_from(
            "/h",
            &[
                ("Downloads/中文 安裝檔.dmg", 60 << 20),
                ("Downloads/old-video.mov", 60 << 20),
                ("Downloads/new.zip", 60 << 20),
            ],
        );
        let old = NOW - 100 * DAY;
        t.node_mut(find(&t, "Downloads/中文 安裝檔.dmg")).mtime = old;
        t.node_mut(find(&t, "Downloads/old-video.mov")).mtime = old;
        t.node_mut(find(&t, "Downloads/new.zip")).mtime = NOW;
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[
                ("Downloads/中文 安裝檔.dmg", "installers"),
                ("Downloads/old-video.mov", "old-downloads")
            ])
        );
    }

    #[test]
    fn large_files_are_found_outside_protected_trees() {
        let t = tree_from(
            "/h",
            &[
                ("Movies/raw.mov", 2 << 30),
                (
                    "Library/Mobile Documents/com~apple~CloudDocs/big.zip",
                    2 << 30,
                ),
            ],
        );
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[("Movies/raw.mov", "large-files")])
        );
    }

    #[test]
    fn docker_is_tool_managed_and_not_descended() {
        let t = tree_from(
            "/h",
            &[(
                "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
                4 << 30,
            )],
        );
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[("Library/Containers/com.docker.docker", "docker")])
        );
    }

    #[test]
    fn scan_root_below_home_uses_home_relative_paths() {
        let t = tree_from(
            "/h/Projects",
            &[("web/package.json", 1), ("web/node_modules/a", 1)],
        );
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[("Projects/web/node_modules", "node-modules")])
        );
    }

    #[test]
    fn scan_root_outside_home_yields_nothing() {
        let t = tree_from(
            "/other",
            &[("web/package.json", 1), ("web/node_modules/a", 1)],
        );
        assert!(eval(&t).is_empty());
    }

    #[test]
    fn suggestions_are_sorted_by_size_desc() {
        let t = tree_from("/h", &[(".cache/small/x", 1), (".cache/big/x", 100)]);
        let s = eval(&t);
        assert_eq!(s.iter().map(|x| x.size).collect::<Vec<_>>(), [100, 1]);
    }
}
