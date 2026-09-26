# disk-sweeper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立一個 Rust 寫的 macOS TUI 硬碟空間清理器：平行掃描 `$HOME`、瀏覽空間佔用、依內建規則建議可清理項目，並經過型別層級強制的安全關卡後才刪除。

**Architecture:** 單一 crate（lib + bin）。掃描在背景執行緒用 `jwalk` 平行走訪並建立 arena 目錄樹，透過 `mpsc` 把進度送給 UI 執行緒。規則引擎在樹上比對出建議；所有刪除都必須先經過 `safety::review` 產生 `ApprovedTarget`（只有 `safety` 模組能建立），`action::execute` 只接受這個型別。

**Tech Stack:** Rust stable（edition 2021）、ratatui 0.29（內含 crossterm）、jwalk 0.8、clap 4、trash 5、serde/serde_json、chrono、libc、unicode-width；測試用 tempfile、proptest、insta。

**Spec:** `docs/superpowers/specs/2026-09-26-disk-sweeper-design.md`

## Global Constraints

- 只支援 macOS；Rust stable、edition 2021
- 永不使用 sudo；只處理 `$HOME` 之下的路徑
- 所有檔案系統變更都必須透過 `safety::review` → `ApprovedTarget` → `action::execute`
- 測試不得讀寫真正的 `$HOME`（一律注入 `Context`），不得呼叫真正的垃圾桶（使用 `Trasher` fake）
- 使用者可見文字一律用台灣正體中文
- `trash` 失敗時回報錯誤，**不可**退而改為直接刪除
- 紀錄檔：`~/Library/Logs/disk-sweeper/actions.jsonl`；非 dry-run 時紀錄檔無法開啟就拒絕執行
- 閾值：總量 > 10 GB 或含任何「移到垃圾桶」項目 → 需輸入 `yes`；Suggest 預設隱藏 < 10 MB；`large-files` ≥ 1 GB；`old-downloads` > 90 天且 ≥ 50 MB
- 大小一律用 `st_blocks * 512`；硬連結以 `(dev, inode)` 只算一次
- 效能目標：`--scan-only` 掃描 `$HOME` 的時間 ≤ `du -sk ~` 的 1/3

**與 spec 的差異（本計畫同步更新 spec）：**
- `PROTECTED_SELF` 另外加入 `Library/Logs`、`Library/Group Containers`；`PROTECTED_TREE` 另外加入 `Library/Logs/disk-sweeper`（自己的紀錄檔）
- `node-modules` / `rust-target` 規則：路徑中有 `.` 開頭的目錄或 `Library` 就不比對（避免刪到全域安裝的 npm 套件與編輯器擴充套件的依賴）
- 垃圾桶使用 `trash` crate 在 macOS 上的預設 Finder 方式（支援「放回原處」；第一次使用時 macOS 會詢問是否允許終端機控制 Finder）
- 風險等級用彩色文字標籤而非 emoji（emoji 在終端機的寬度不一致會破壞對齊）
- 新增 `--scan-only`：只掃描並印出摘要，用來量測效能目標

## Review Focus

1. **大小寫不同的禁區路徑**（`~/library/KEYCHAINS/x`、`~/.SSH/id`）：APFS 預設不分大小寫，這些路徑指向同一個地方，必須被拒絕 → Task 4 `case_variants_of_protected_paths_are_rejected`
2. **全域安裝套件或編輯器擴充套件裡的 `node_modules`**（`~/.nvm/.../lib/node_modules/pnpm/node_modules`、`~/.vscode/extensions/x/node_modules`）：絕不能建議為「可重建」 → Task 5 `node_modules_under_hidden_or_library_dirs_are_ignored`
3. **同時標記父目錄與子項目**：不能重複計算大小，也不能重複刪除 → Task 7 `marking_parent_and_child_does_not_double_count`
4. **掃描後到執行前，檔案被 App 改寫或已經消失**：要略過並說明原因，不能當成失敗或 panic → Task 6 `changed_after_scan_is_skipped`、`vanished_path_is_skipped`
5. **名稱含空白、CJK、emoji 的路徑**（`Application Support`、`中文 安裝檔.dmg`）：路徑組合、規則比對、顯示都要正確 → Task 2 `path_of_handles_spaces_and_cjk`、Task 5 `installers_win_over_old_downloads`、Task 8 `pad_uses_display_width`

---

## File Structure

```
Cargo.toml
src/
  main.rs              CLI 解析、啟動掃描執行緒、TUI 事件迴圈、執行清理
  lib.rs               模組宣告
  context.rs           Context { home, path_var }（可注入）
  size.rs              format_size / parse_size
  tree.rs              arena 目錄樹 + testutil
  scan/mod.rs          平行掃描、進度事件、TCC 偵測
  risk.rs              Risk / Handling / ToolCommand（safety、rules、action 共用）
  safety/mod.rs        review()、Candidate、ApprovedTarget、Verdict、find_in_path
  safety/forbidden.rs  硬性禁區清單與比對
  rules/mod.rs         Category、Matcher、Rule、Suggestion、evaluate()
  rules/catalog.rs     規則清單（依優先順序）
  action/mod.rs        execute()、Trasher、Outcome、ActionResult
  action/journal.rs    ActionLog（JSONL）
  action/procs.rs      執行中 App 偵測（pgrep）
  disk.rs              statfs → DiskInfo
  app.rs               App 狀態機（按鍵、標記、確認流程），不做 I/O 繪圖
  ui/mod.rs            draw()、header/footer、共用 helper
  ui/explore.rs        Explore 畫面
  ui/suggest.rs        Suggest 畫面
  ui/confirm.rs        確認對話框
  ui/result.rs         結果畫面
  ui/overlay.rs        掃描中 / 執行中 / 說明 / 錯誤清單
```

---

### Task 1: Toolchain、專案骨架、Context、size

**Files:**
- Create: `Cargo.toml`, `src/lib.rs`, `src/context.rs`, `src/size.rs`
- Keep: `src/main.rs`（`cargo init` 產生的 hello world，Task 9 取代）

**Interfaces:**
- Produces:
  - `context::Context { pub home: PathBuf, pub path_var: OsString }`、`Context::new(home: impl AsRef<Path>, path_var: OsString) -> io::Result<Context>`（canonicalize home）、`Context::from_env() -> io::Result<Context>`、`Context::log_dir(&self) -> PathBuf`
  - `size::format_size(u64) -> String`、`size::parse_size(&str) -> Result<u64, String>`

- [ ] **Step 1: 安裝 Rust toolchain**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal -c clippy,rustfmt
. "$HOME/.cargo/env"
cargo --version
```
Expected: 印出 `cargo 1.x.x`。之後的指令若找不到 `cargo`，在前面加上 `. "$HOME/.cargo/env" &&`。

- [ ] **Step 2: 建立 crate**

```bash
cd /Users/stevenshen/Projects/personal/disk-sweeper && cargo init --name disk-sweeper
```

用以下內容覆寫 `Cargo.toml`：

```toml
[package]
name = "disk-sweeper"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = "1"
chrono = "0.4"
clap = { version = "4", features = ["derive"] }
jwalk = "0.8"
libc = "0.2"
ratatui = "0.29"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
trash = "5"
unicode-width = "0.2"

[dev-dependencies]
insta = "1"
proptest = "1"
tempfile = "3"

[profile.release]
lto = true
codegen-units = 1
```

建立 `src/lib.rs`：

```rust
pub mod context;
pub mod size;
```

- [ ] **Step 3: 寫失敗的測試**

`src/size.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_below_one_kib() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(1023), "1023B");
    }

    #[test]
    fn formats_with_one_decimal_below_100() {
        assert_eq!(format_size(1536), "1.5K");
        assert_eq!(format_size(3_865_470_566), "3.6G");
    }

    #[test]
    fn formats_without_decimal_at_or_above_100() {
        assert_eq!(format_size(150 * 1024 * 1024), "150M");
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("64K"), Ok(65_536));
        assert_eq!(parse_size("10"), Ok(10));
        assert_eq!(parse_size("1g"), Ok(1 << 30));
        assert_eq!(parse_size("2MB"), Ok(2 << 20));
    }

    #[test]
    fn rejects_invalid_sizes() {
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("5X").is_err());
    }
}
```

`src/context.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_canonicalizes_home() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Context::new(dir.path(), OsString::new()).unwrap();
        assert_eq!(ctx.home, dir.path().canonicalize().unwrap());
        assert!(ctx.log_dir().ends_with("Library/Logs/disk-sweeper"));
    }

    #[test]
    fn new_fails_for_missing_home() {
        assert!(Context::new("/definitely/not/here", OsString::new()).is_err());
    }
}
```

- [ ] **Step 4: 確認測試失敗**

Run: `cargo test --lib`
Expected: 編譯失敗，錯誤為 `cannot find function format_size` / `cannot find type Context`

- [ ] **Step 5: 實作**

在 `src/size.rs` 測試模組上方加入：

```rust
const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];

/// 以 1024 為底的人類可讀大小：小於 100 顯示一位小數。
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{:.0}{}", value, UNITS[unit])
    } else {
        format!("{:.1}{}", value, UNITS[unit])
    }
}

/// 解析 "64K"、"10"、"1G"、"2MB"（不分大小寫）。
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().map_err(|_| format!("無效的大小：{s}"))?;
    let mult: u64 = match unit.to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" => 1 << 10,
        "M" | "MB" => 1 << 20,
        "G" | "GB" => 1 << 30,
        _ => return Err(format!("無效的單位：{unit}")),
    };
    n.checked_mul(mult).ok_or_else(|| format!("大小溢位：{s}"))
}
```

在 `src/context.rs` 測試模組上方加入：

```rust
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// 執行環境。測試時注入假的 home，永遠不碰真正的 $HOME。
#[derive(Debug, Clone)]
pub struct Context {
    /// canonicalize 過的 home 目錄
    pub home: PathBuf,
    /// 用來尋找外部指令的 PATH
    pub path_var: OsString,
}

impl Context {
    pub fn new(home: impl AsRef<Path>, path_var: OsString) -> io::Result<Self> {
        Ok(Self { home: home.as_ref().canonicalize()?, path_var })
    }

    pub fn from_env() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        Self::new(home, std::env::var_os("PATH").unwrap_or_default())
    }

    pub fn log_dir(&self) -> PathBuf {
        self.home.join("Library/Logs/disk-sweeper")
    }
}
```

- [ ] **Step 6: 確認測試通過**

Run: `cargo test --lib`
Expected: 7 passed

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock .gitignore src/
git commit -m "feat: scaffold crate with Context and size helpers"
```

---

### Task 2: arena 目錄樹

**Files:**
- Create: `src/tree.rs`
- Modify: `src/lib.rs`（加入 `pub mod tree;`）

**Interfaces:**
- Produces:
  - `tree::NodeId`（`Copy + Ord + Hash`，`index() -> usize`）
  - `tree::NodeKind { Dir, File, Symlink, Other }`
  - `tree::NodeMeta { pub size: u64, pub inode: u64, pub dev: u64, pub mtime: i64 }`（`Copy + Default`）
  - `tree::Node { pub name: Box<str>, pub parent: Option<NodeId>, pub children: Vec<NodeId>, pub kind, pub size, pub file_count, pub inode, pub dev, pub mtime }`
  - `Tree::new(root_path: PathBuf, meta: NodeMeta)`、`root()`、`root_path() -> &Path`、`len()`、`node(id) -> &Node`、`node_mut(id) -> &mut Node`、`add(parent, name: &str, kind, meta) -> NodeId`、`add_aggregate(parent, size)`、`finalize()`、`path_of(id) -> PathBuf`、`children_sorted_by_size(id) -> Vec<NodeId>`、`is_ancestor(ancestor, id) -> bool`、`child_named(id, name) -> Option<NodeId>`、`subtract(id)`
  - `#[cfg(test)] tree::testutil::{tree_from(root: &str, files: &[(&str, u64)]) -> Tree, find(&Tree, rel: &str) -> NodeId}`：`tree_from` 自動建立中間目錄（大小 0）、以 `/` 結尾的項目視為空目錄、每個節點的 inode 不同、mtime 為 0，最後呼叫 `finalize()`

- [ ] **Step 1: 寫失敗的測試**

`src/tree.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::testutil::{find, tree_from};
    use super::*;

    #[test]
    fn finalize_accumulates_sizes_and_counts() {
        let t = tree_from("/h", &[("a/x", 100), ("a/b/y", 50), ("z", 1)]);
        assert_eq!(t.node(t.root()).size, 151);
        assert_eq!(t.node(t.root()).file_count, 3);
        let a = find(&t, "a");
        assert_eq!(t.node(a).size, 150);
        assert_eq!(t.node(a).file_count, 2);
    }

    #[test]
    fn path_of_handles_spaces_and_cjk() {
        let rel = "Library/Application Support/中文 檔案 🎉.txt";
        let t = tree_from("/h", &[(rel, 1)]);
        assert_eq!(t.path_of(find(&t, rel)), PathBuf::from("/h").join(rel));
        assert_eq!(t.path_of(t.root()), PathBuf::from("/h"));
    }

    #[test]
    fn children_sorted_by_size_desc_then_name() {
        let t = tree_from("/h", &[("small", 1), ("big", 10), ("also-big", 10)]);
        let names: Vec<&str> =
            t.children_sorted_by_size(t.root()).iter().map(|&c| &*t.node(c).name).collect();
        assert_eq!(names, ["also-big", "big", "small"]);
    }

    #[test]
    fn subtract_updates_ancestors_and_is_idempotent() {
        let mut t = tree_from("/h", &[("a/b/x", 100), ("a/y", 10)]);
        let a = find(&t, "a");
        let b = find(&t, "a/b");
        t.subtract(b);
        t.subtract(b);
        assert_eq!(t.node(t.root()).size, 10);
        assert_eq!(t.node(t.root()).file_count, 1);
        assert_eq!(t.node(a).size, 10);
        assert!(!t.node(a).children.contains(&b));
    }

    #[test]
    fn is_ancestor_follows_parent_chain() {
        let t = tree_from("/h", &[("a/b/x", 1), ("c", 1)]);
        let (a, x, c) = (find(&t, "a"), find(&t, "a/b/x"), find(&t, "c"));
        assert!(t.is_ancestor(a, x));
        assert!(!t.is_ancestor(x, a));
        assert!(!t.is_ancestor(c, x));
        assert!(!t.is_ancestor(a, a));
    }

    #[test]
    fn add_aggregate_counts_into_parent() {
        let mut t = Tree::new("/h".into(), NodeMeta::default());
        let root = t.root();
        t.add_aggregate(root, 7);
        t.finalize();
        assert_eq!(t.node(root).size, 7);
        assert_eq!(t.node(root).file_count, 1);
        assert!(t.node(root).children.is_empty());
    }
}
```

- [ ] **Step 2: 確認測試失敗**

Run: `cargo test --lib tree`
Expected: 編譯失敗（`Tree`、`testutil` 未定義）

- [ ] **Step 3: 實作**

在 `src/tree.rs` 測試模組上方加入：

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NodeMeta {
    pub size: u64,
    pub inode: u64,
    pub dev: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: Box<str>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub kind: NodeKind,
    /// finalize 之後為整個子樹的佔用空間
    pub size: u64,
    pub file_count: u64,
    pub inode: u64,
    pub dev: u64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
pub struct Tree {
    root_path: PathBuf,
    nodes: Vec<Node>,
}

impl Tree {
    pub fn new(root_path: PathBuf, meta: NodeMeta) -> Self {
        let root = Node {
            name: "".into(),
            parent: None,
            children: Vec::new(),
            kind: NodeKind::Dir,
            size: meta.size,
            file_count: 0,
            inode: meta.inode,
            dev: meta.dev,
            mtime: meta.mtime,
        };
        Self { root_path, nodes: vec![root] }
    }

    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    pub fn add(&mut self, parent: NodeId, name: &str, kind: NodeKind, meta: NodeMeta) -> NodeId {
        let id = NodeId(u32::try_from(self.nodes.len()).expect("too many nodes"));
        self.nodes.push(Node {
            name: name.into(),
            parent: Some(parent),
            children: Vec::new(),
            kind,
            size: meta.size,
            file_count: u64::from(kind != NodeKind::Dir),
            inode: meta.inode,
            dev: meta.dev,
            mtime: meta.mtime,
        });
        self.nodes[parent.index()].children.push(id);
        id
    }

    /// 不建立節點的小檔案：直接計入父目錄。
    pub fn add_aggregate(&mut self, parent: NodeId, size: u64) {
        let node = &mut self.nodes[parent.index()];
        node.size += size;
        node.file_count += 1;
    }

    /// 由葉往根累加 size 與 file_count。子節點的 id 一定大於父節點，倒序走一次即可。
    /// 只能在建樹完成後呼叫一次。
    pub fn finalize(&mut self) {
        for i in (1..self.nodes.len()).rev() {
            let (size, count, parent) = {
                let n = &self.nodes[i];
                (n.size, n.file_count, n.parent)
            };
            if let Some(p) = parent {
                let pn = &mut self.nodes[p.index()];
                pn.size += size;
                pn.file_count += count;
            }
        }
    }

    pub fn path_of(&self, id: NodeId) -> PathBuf {
        let mut names = Vec::new();
        let mut cur = id;
        while let Some(parent) = self.nodes[cur.index()].parent {
            names.push(&*self.nodes[cur.index()].name);
            cur = parent;
        }
        let mut path = self.root_path.clone();
        for name in names.iter().rev() {
            path.push(name);
        }
        path
    }

    pub fn children_sorted_by_size(&self, id: NodeId) -> Vec<NodeId> {
        let mut children = self.nodes[id.index()].children.clone();
        children.sort_by(|a, b| {
            let (na, nb) = (self.node(*a), self.node(*b));
            nb.size.cmp(&na.size).then_with(|| na.name.cmp(&nb.name))
        });
        children
    }

    pub fn is_ancestor(&self, ancestor: NodeId, id: NodeId) -> bool {
        let mut cur = self.nodes[id.index()].parent;
        while let Some(p) = cur {
            if p == ancestor {
                return true;
            }
            cur = self.nodes[p.index()].parent;
        }
        false
    }

    pub fn child_named(&self, id: NodeId, name: &str) -> Option<NodeId> {
        self.nodes[id.index()].children.iter().copied().find(|c| &*self.node(*c).name == name)
    }

    /// 刪除成功後呼叫：從父節點移除並從所有祖先扣除大小。重複呼叫不會重複扣除。
    pub fn subtract(&mut self, id: NodeId) {
        let Some(parent) = self.node(id).parent else { return };
        if !self.nodes[parent.index()].children.contains(&id) {
            return;
        }
        let (size, count) = (self.node(id).size, self.node(id).file_count);
        self.nodes[parent.index()].children.retain(|c| *c != id);
        let mut cur = Some(parent);
        while let Some(p) = cur {
            let n = &mut self.nodes[p.index()];
            n.size = n.size.saturating_sub(size);
            n.file_count = n.file_count.saturating_sub(count);
            cur = n.parent;
        }
    }
}

#[cfg(test)]
pub mod testutil {
    use super::*;

    /// 依 (相對路徑, 大小) 建立已 finalize 的樹。中間目錄自動建立（大小 0）；
    /// 以 "/" 結尾的項目視為空目錄。每個節點的 inode 都不同，mtime 為 0。
    pub fn tree_from(root: &str, files: &[(&str, u64)]) -> Tree {
        let mut t = Tree::new(PathBuf::from(root), NodeMeta::default());
        for (rel, size) in files {
            let is_dir = rel.ends_with('/');
            let parts: Vec<&str> = rel.trim_end_matches('/').split('/').collect();
            let mut cur = t.root();
            for (i, part) in parts.iter().enumerate() {
                let last = i == parts.len() - 1;
                cur = match t.child_named(cur, part) {
                    Some(c) => c,
                    None => {
                        let kind = if last && !is_dir { NodeKind::File } else { NodeKind::Dir };
                        let inode = t.len() as u64;
                        let size = if last { *size } else { 0 };
                        t.add(cur, part, kind, NodeMeta { size, inode, dev: 1, mtime: 0 })
                    }
                };
            }
        }
        t.finalize();
        t
    }

    pub fn find(t: &Tree, rel: &str) -> NodeId {
        rel.split('/').fold(t.root(), |cur, part| {
            t.child_named(cur, part).unwrap_or_else(|| panic!("no node {rel}"))
        })
    }
}
```

`src/lib.rs` 加入 `pub mod tree;`。

- [ ] **Step 4: 確認測試通過**

Run: `cargo test --lib tree`
Expected: 6 passed

- [ ] **Step 5: Commit**

```bash
git add src/tree.rs src/lib.rs
git commit -m "feat: add arena directory tree"
```

---

### Task 3: 平行掃描引擎

**Files:**
- Create: `src/scan/mod.rs`
- Modify: `src/lib.rs`（加入 `pub mod scan;`）

**Interfaces:**
- Consumes: `tree::{Tree, NodeId, NodeKind, NodeMeta}`
- Produces:
  - `scan::ScanOptions { pub threads: usize, pub min_node_size: u64 }`
  - `scan::ScanError { pub path: PathBuf, pub message: String, pub permission_denied: bool }`
  - `scan::Progress { pub files: u64, pub bytes: u64, pub errors: u64, pub current: PathBuf, pub top: Vec<(String, u64)> }`（`Default + Clone`）
  - `scan::ScanResult { pub tree: Tree, pub errors: Vec<ScanError>, pub elapsed: Duration }`
  - `scan::ScanEvent { Progress(Progress), Done(ScanResult) }`
  - `scan::scan(root: &Path, opts: &ScanOptions, tx: &Sender<ScanEvent>) -> ScanResult`（同步執行；只送 `Progress`，`Done` 由呼叫端送出）
  - `scan::needs_full_disk_access(errors: &[ScanError], home: &Path) -> bool`

- [ ] **Step 1: 寫失敗的測試**

`src/scan/mod.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use tempfile::TempDir;

    fn root() -> (TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().canonicalize().unwrap();
        (d, p)
    }

    fn write(p: &Path, len: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; len]).unwrap();
    }

    fn blocks(p: &Path) -> u64 {
        fs::symlink_metadata(p).unwrap().blocks() * 512
    }

    fn run(root: &Path, min_node_size: u64) -> ScanResult {
        let (tx, _rx) = mpsc::channel();
        scan(root, &ScanOptions { threads: 4, min_node_size }, &tx)
    }

    #[test]
    fn sums_allocated_sizes() {
        let (_d, r) = root();
        write(&r.join("a/x"), 10_000);
        write(&r.join("a/b/y"), 5_000);
        write(&r.join("z"), 1);
        let res = run(&r, 0);
        let expected: u64 = ["", "a", "a/b", "a/x", "a/b/y", "z"]
            .iter()
            .map(|rel| blocks(&if rel.is_empty() { r.clone() } else { r.join(rel) }))
            .sum();
        let t = &res.tree;
        assert_eq!(t.node(t.root()).size, expected);
        assert_eq!(t.node(t.root()).file_count, 3);
        let a = t.child_named(t.root(), "a").unwrap();
        assert_eq!(t.node(a).file_count, 2);
        assert!(res.errors.is_empty());
    }

    #[test]
    fn hard_links_are_counted_once() {
        let (_d, r) = root();
        write(&r.join("f"), 100_000);
        fs::hard_link(r.join("f"), r.join("g")).unwrap();
        let res = run(&r, 0);
        let t = &res.tree;
        assert_eq!(t.node(t.root()).size, blocks(&r) + blocks(&r.join("f")));
        assert_eq!(t.node(t.root()).file_count, 2);
    }

    #[test]
    fn permission_errors_are_recorded_and_scan_continues() {
        let (_d, r) = root();
        write(&r.join("locked/secret"), 10);
        write(&r.join("open/ok"), 10);
        fs::set_permissions(r.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let res = run(&r, 0);
        fs::set_permissions(r.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            res.errors.iter().any(|e| e.permission_denied && e.path.starts_with(r.join("locked"))),
            "{:?}",
            res.errors
        );
        assert!(res.tree.child_named(res.tree.root(), "open").is_some());
    }

    #[test]
    fn small_files_are_aggregated_into_parent() {
        let (_d, r) = root();
        write(&r.join("d/small"), 10);
        write(&r.join("d/big"), 100_000);
        let res = run(&r, 64 * 1024);
        let t = &res.tree;
        let d = t.child_named(t.root(), "d").unwrap();
        assert!(t.child_named(d, "small").is_none());
        assert!(t.child_named(d, "big").is_some());
        assert_eq!(t.node(d).file_count, 2);
        assert_eq!(
            t.node(d).size,
            blocks(&r.join("d")) + blocks(&r.join("d/small")) + blocks(&r.join("d/big"))
        );
    }

    #[test]
    fn symlinks_are_not_followed() {
        let (_d, r) = root();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("huge"), 200_000);
        std::os::unix::fs::symlink(outside.path(), r.join("link")).unwrap();
        let res = run(&r, 0);
        let t = &res.tree;
        let link = t.child_named(t.root(), "link").unwrap();
        assert_eq!(t.node(link).kind, NodeKind::Symlink);
        assert_eq!(t.node(t.root()).size, blocks(&r) + blocks(&r.join("link")));
    }

    #[test]
    fn missing_root_yields_error_not_panic() {
        let res = run(Path::new("/definitely/not/here"), 0);
        assert_eq!(res.errors.len(), 1);
        assert!(res.tree.is_empty());
    }

    #[test]
    fn detects_tcc_protected_failures() {
        let home = Path::new("/h");
        let err = |p: &str| ScanError { path: p.into(), message: "x".into(), permission_denied: true };
        assert!(needs_full_disk_access(&[err("/h/Library/Mail/V10")], home));
        assert!(!needs_full_disk_access(&[err("/h/Projects/x")], home));
    }
}
```

- [ ] **Step 2: 確認測試失敗**

Run: `cargo test --lib scan`
Expected: 編譯失敗（`scan`、`ScanOptions` 未定義）

- [ ] **Step 3: 實作**

在 `src/scan/mod.rs` 測試模組上方加入：

```rust
use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use jwalk::{Parallelism, WalkDirGeneric};

use crate::tree::{NodeId, NodeKind, NodeMeta, Tree};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// 受 macOS TCC 保護、沒有「完全取用磁碟」權限時會讀取失敗的路徑（相對於 home）
const TCC_PATHS: &[&str] = &[
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Calendars",
    "Library/Reminders",
    "Library/Suggestions",
    ".Trash",
];

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub threads: usize,
    /// 小於此大小的檔案不建立節點，只計入父目錄
    pub min_node_size: u64,
}

#[derive(Debug, Clone)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
    pub permission_denied: bool,
}

impl ScanError {
    fn from_io(path: &Path, e: &io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            message: e.to_string(),
            permission_denied: e.kind() == io::ErrorKind::PermissionDenied,
        }
    }

    fn from_jwalk(e: &jwalk::Error) -> Self {
        let path = e.path().map(Path::to_path_buf).unwrap_or_default();
        match e.io_error() {
            Some(io_err) => Self::from_io(&path, io_err),
            None => Self { path, message: e.to_string(), permission_denied: false },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub files: u64,
    pub bytes: u64,
    pub errors: u64,
    pub current: PathBuf,
    /// 目前累計最大的第一層項目
    pub top: Vec<(String, u64)>,
}

#[derive(Debug)]
pub struct ScanResult {
    pub tree: Tree,
    pub errors: Vec<ScanError>,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum ScanEvent {
    Progress(Progress),
    Done(ScanResult),
}

#[derive(Debug, Clone, Copy)]
struct EntryMeta {
    kind: NodeKind,
    meta: NodeMeta,
    nlink: u64,
}

fn entry_meta(m: &std::fs::Metadata) -> EntryMeta {
    let ft = m.file_type();
    let kind = if ft.is_dir() {
        NodeKind::Dir
    } else if ft.is_file() {
        NodeKind::File
    } else if ft.is_symlink() {
        NodeKind::Symlink
    } else {
        NodeKind::Other
    };
    EntryMeta {
        kind,
        meta: NodeMeta { size: m.blocks() * 512, inode: m.ino(), dev: m.dev(), mtime: m.mtime() },
        nlink: m.nlink(),
    }
}

pub fn scan(root: &Path, opts: &ScanOptions, tx: &Sender<ScanEvent>) -> ScanResult {
    let start = Instant::now();
    let root_meta = match std::fs::symlink_metadata(root) {
        Ok(m) => entry_meta(&m),
        Err(e) => {
            return ScanResult {
                tree: Tree::new(root.to_path_buf(), NodeMeta::default()),
                errors: vec![ScanError::from_io(root, &e)],
                elapsed: start.elapsed(),
            };
        }
    };
    let root_dev = root_meta.meta.dev;
    let mut tree = Tree::new(root.to_path_buf(), root_meta.meta);
    let mut errors = Vec::new();
    let mut dirs: HashMap<PathBuf, NodeId> = HashMap::from([(root.to_path_buf(), tree.root())]);
    let mut hard_links: HashSet<(u64, u64)> = HashSet::new();
    let mut top: HashMap<String, u64> = HashMap::new();
    let mut progress = Progress::default();
    let mut last_emit = Instant::now();

    let walk = WalkDirGeneric::<((), Option<EntryMeta>)>::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(Parallelism::RayonNewPool(opts.threads.max(1)))
        .process_read_dir(move |_depth, _dir, _state, children| {
            // 在 rayon 執行緒上平行 lstat；不同檔案系統的目錄不往下走
            for child in children.iter_mut().flatten() {
                if let Ok(m) = std::fs::symlink_metadata(child.path()) {
                    let em = entry_meta(&m);
                    if em.kind == NodeKind::Dir && em.meta.dev != root_dev {
                        child.read_children_path = None;
                    }
                    child.client_state = Some(em);
                }
            }
        });

    for entry in walk {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errors.push(ScanError::from_jwalk(&e));
                progress.errors += 1;
                continue;
            }
        };
        if entry.depth == 0 {
            continue;
        }
        let path = entry.path();
        let Some(em) = entry.client_state else {
            errors.push(ScanError { path, message: "無法讀取 metadata".into(), permission_denied: false });
            progress.errors += 1;
            continue;
        };
        let Some(&parent) = dirs.get(&*entry.parent_path) else { continue };

        let mut meta = em.meta;
        if em.kind == NodeKind::File && em.nlink > 1 && !hard_links.insert((meta.dev, meta.inode)) {
            meta.size = 0;
        }
        let name = entry.file_name.to_string_lossy();
        if em.kind == NodeKind::Dir {
            let id = tree.add(parent, &name, em.kind, meta);
            dirs.insert(path.clone(), id);
        } else if meta.size < opts.min_node_size {
            tree.add_aggregate(parent, meta.size);
        } else {
            tree.add(parent, &name, em.kind, meta);
        }

        progress.files += 1;
        progress.bytes += meta.size;
        if let Some(first) = path.strip_prefix(root).ok().and_then(|r| r.components().next()) {
            let key = first.as_os_str().to_string_lossy();
            match top.get_mut(key.as_ref()) {
                Some(v) => *v += meta.size,
                None => {
                    top.insert(key.into_owned(), meta.size);
                }
            }
        }
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            last_emit = Instant::now();
            progress.current = path;
            progress.top = top_n(&top, 5);
            let _ = tx.send(ScanEvent::Progress(progress.clone()));
        }
    }

    tree.finalize();
    ScanResult { tree, errors, elapsed: start.elapsed() }
}

fn top_n(top: &HashMap<String, u64>, n: usize) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = top.iter().map(|(k, s)| (k.clone(), *s)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(n);
    v
}

/// 是否有 TCC 保護路徑因權限不足而讀取失敗（需提示使用者授權「完全取用磁碟」）。
pub fn needs_full_disk_access(errors: &[ScanError], home: &Path) -> bool {
    errors.iter().filter(|e| e.permission_denied).any(|e| {
        e.path.strip_prefix(home).is_ok_and(|rel| TCC_PATHS.iter().any(|t| rel.starts_with(t)))
    })
}
```

`src/lib.rs` 加入 `pub mod scan;`。

- [ ] **Step 4: 確認測試通過**

Run: `cargo test --lib scan`
Expected: 7 passed。若 `permission_errors_are_recorded_and_scan_continues` 失敗，印出的 `errors` 會顯示 jwalk 回報的實際路徑；修正斷言讓它比對真正被拒絕讀取的路徑（仍須要求 `permission_denied == true`），不可刪除這個測試。

- [ ] **Step 5: Commit**

```bash
git add src/scan src/lib.rs
git commit -m "feat: add parallel scanner with progress events"
```

---

### Task 4: 風險型別與安全守門員

**Files:**
- Create: `src/risk.rs`, `src/safety/mod.rs`, `src/safety/forbidden.rs`
- Modify: `src/lib.rs`（加入 `pub mod risk;`、`pub mod safety;`）

**Interfaces:**
- Consumes: `context::Context`
- Produces:
  - `risk::Risk { Regenerable, UserData, Protected, ToolManaged }`（`Copy + Eq + Hash + Debug`），`Risk::label(self) -> &'static str`
  - `risk::ToolCommand { pub program: &'static str, pub args: &'static [&'static str] }`（`Copy + Eq`），`display(&self) -> String`
  - `risk::Handling { Delete, Trash, Command(ToolCommand) }`（`Copy + Eq`），`label(&self) -> &'static str`
  - `safety::Candidate { pub path, pub risk, pub rule_id: Option<&'static str>, pub command: Option<ToolCommand>, pub size: u64, pub inode: u64, pub mtime: i64 }`
  - `safety::ApprovedTarget`（欄位私有；getter：`path() -> &Path`、`handling() -> Handling`、`rule_id() -> Option<&'static str>`、`size()`、`inode()`、`mtime()`）
  - `safety::Verdict { Approved(ApprovedTarget), Rejected { path: PathBuf, reason: String } }`
  - `safety::review(c: Candidate, ctx: &Context) -> Verdict`
  - `safety::protection_reason(path: &Path, ctx: &Context) -> Option<&'static str>`（純字串比對，UI 用）
  - `safety::find_in_path(program: &str, path_var: &OsStr) -> Option<PathBuf>`
  - `safety::forbidden::{protection_reason_rel(rel: &str) -> Option<&'static str>, is_protected_tree_rel(rel: &str) -> bool}`

- [ ] **Step 1: 寫 `src/risk.rs`（純資料型別，無邏輯需要測試以外的行為）**

```rust
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
```

- [ ] **Step 2: 寫失敗的測試**

`src/safety/forbidden.rs`：

```rust
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
```

`src/safety/mod.rs`：

```rust
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
            "Projects/repo/.git",
            "Pictures/Photos Library.photoslibrary",
            "bin",
        ] {
            fs::create_dir_all(d.path().join(p)).unwrap();
        }
        let bin = d.path().join("bin/fakebrew");
        fs::write(&bin, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        let ctx = Context::new(d.path(), d.path().join("bin").into_os_string()).unwrap();
        (d, ctx)
    }

    fn cand(path: PathBuf, risk: Risk, rule: Option<&'static str>) -> Candidate {
        Candidate { path, risk, rule_id: rule, command: None, size: 1, inode: 1, mtime: 0 }
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
            let p = if rel.is_empty() { ctx.home.clone() } else { ctx.home.join(rel) };
            assert!(rejected(&review(cand(p, Risk::Regenerable, Some("r")), &ctx)), "{rel}");
        }
    }

    #[test]
    fn regenerable_with_rule_is_deleted() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Library/Caches/com.example");
        assert_eq!(handling(review(cand(p, Risk::Regenerable, Some("app-caches")), &ctx)), Handling::Delete);
    }

    #[test]
    fn regenerable_without_rule_is_downgraded_to_trash() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Library/Caches/com.example");
        assert_eq!(handling(review(cand(p, Risk::Regenerable, None), &ctx)), Handling::Trash);
    }

    #[test]
    fn user_data_goes_to_trash() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Documents/report.pdf");
        assert_eq!(handling(review(cand(p, Risk::UserData, None), &ctx)), Handling::Trash);
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
        for rel in ["Library/Keychains/login.keychain-db", ".ssh/id_ed25519", "Library/Mail/V10", ".config/fish"] {
            assert!(rejected(&review(cand(ctx.home.join(rel), Risk::UserData, None), &ctx)), "{rel}");
        }
    }

    #[test]
    fn case_variants_of_protected_paths_are_rejected() {
        let (_d, ctx) = setup();
        for rel in ["library/KEYCHAINS/x", ".SSH/id", "LIBRARY", "library/caches"] {
            assert!(rejected(&review(cand(ctx.home.join(rel), Risk::Regenerable, Some("r")), &ctx)), "{rel}");
        }
    }

    #[test]
    fn outside_home_relative_and_dotdot_paths_are_rejected() {
        let (_d, ctx) = setup();
        for p in [PathBuf::from("/etc/hosts"), PathBuf::from("relative/x"), ctx.home.join("Projects/../.ssh/id")] {
            assert!(rejected(&review(cand(p.clone(), Risk::UserData, None), &ctx)), "{p:?}");
        }
    }

    #[test]
    fn symlink_into_protected_dir_is_rejected_but_link_itself_is_allowed() {
        let (_d, ctx) = setup();
        let link = ctx.home.join("sneaky");
        std::os::unix::fs::symlink(ctx.home.join(".ssh"), &link).unwrap();
        assert!(rejected(&review(cand(link.join("id_rsa"), Risk::UserData, None), &ctx)));
        match review(cand(link.clone(), Risk::UserData, None), &ctx) {
            Verdict::Approved(t) => {
                assert_eq!(t.path(), link.as_path());
                assert_eq!(t.handling(), Handling::Trash);
            }
            Verdict::Rejected { reason, .. } => panic!("{reason}"),
        }
    }

    #[test]
    fn git_internals_and_photos_library_are_rejected() {
        let (_d, ctx) = setup();
        for rel in ["Projects/repo/.git", "Projects/repo/.git/objects", "Pictures/Photos Library.photoslibrary"] {
            assert!(rejected(&review(cand(ctx.home.join(rel), Risk::UserData, None), &ctx)), "{rel}");
        }
    }

    #[test]
    fn tool_managed_requires_available_command() {
        let (_d, ctx) = setup();
        let p = ctx.home.join("Library/Caches/com.example");
        assert!(rejected(&review(cand(p.clone(), Risk::ToolManaged, Some("t")), &ctx)));
        let mut missing = cand(p.clone(), Risk::ToolManaged, Some("t"));
        missing.command = Some(ToolCommand { program: "nope-not-here", args: &[] });
        assert!(rejected(&review(missing, &ctx)));
        let mut ok = cand(p, Risk::ToolManaged, Some("t"));
        let cmd = ToolCommand { program: "fakebrew", args: &["cleanup"] };
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
            name in "[a-zA-Z0-9 ._中文-]{1,20}",
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
```

- [ ] **Step 3: 確認測試失敗**

`src/lib.rs` 加入 `pub mod risk;` 與 `pub mod safety;`，然後：

Run: `cargo test --lib safety`
Expected: 編譯失敗（`review`、`within_ci` 等未定義）

- [ ] **Step 4: 實作 `src/safety/forbidden.rs`**（加在測試模組上方）

```rust
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
```

- [ ] **Step 5: 實作 `src/safety/mod.rs`**（加在測試模組上方）

```rust
//! 安全守門員：所有檔案系統變更的唯一關卡。
//! `ApprovedTarget` 的欄位私有，只能由本模組的 `review` 建立，
//! 而 `action::execute` 只接受 `ApprovedTarget`，所以從型別上無法繞過檢查。

pub mod forbidden;

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
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
        Err(reason) => Verdict::Rejected { path: c.path, reason },
    }
}

fn check(c: &Candidate, ctx: &Context) -> Result<(PathBuf, Handling), String> {
    let resolved = resolve(&c.path)?;
    let rel = resolved.strip_prefix(&ctx.home).map_err(|_| "不在家目錄之下".to_string())?;
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
            let cmd = c.command.ok_or("此項目沒有可自動執行的官方指令，請手動處理")?;
            if find_in_path(cmd.program, &ctx.path_var).is_none() {
                return Err(format!("找不到指令 `{}`", cmd.program));
            }
            Handling::Command(cmd)
        }
    };
    Ok((resolved, handling))
}

/// 解析上層目錄的 symlink（目標本身不跟隨），並拒絕含 `..` 的路徑。
fn resolve(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("必須是絕對路徑".into());
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("路徑不可包含 `..`".into());
    }
    let name = path.file_name().ok_or("路徑沒有檔名")?;
    let parent = path.parent().ok_or("路徑沒有上層目錄")?;
    let parent = parent.canonicalize().map_err(|e| format!("無法解析上層目錄：{e}"))?;
    Ok(parent.join(name))
}

/// 純字串比對（不碰檔案系統），給 UI 標示「受保護」用。
pub fn protection_reason(path: &Path, ctx: &Context) -> Option<&'static str> {
    match path.strip_prefix(&ctx.home) {
        Ok(rel) => forbidden::protection_reason_rel(&rel.to_string_lossy()),
        Err(_) => Some("不在家目錄之下"),
    }
}

pub fn find_in_path(program: &str, path_var: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path_var).map(|dir| dir.join(program)).find(|p| {
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}
```

- [ ] **Step 6: 確認測試通過**

Run: `cargo test --lib -- safety risk`
Expected: 全部通過（forbidden 3 個、safety 12 個 + 2 個 proptest、risk 1 個）

- [ ] **Step 7: Commit**

```bash
git add src/risk.rs src/safety src/lib.rs
git commit -m "feat: add risk types and safety gatekeeper"
```

---

### Task 5: 規則引擎與規則清單

**Files:**
- Create: `src/rules/mod.rs`, `src/rules/catalog.rs`
- Modify: `src/lib.rs`（加入 `pub mod rules;`）

**Interfaces:**
- Consumes: `tree::{Tree, NodeId, NodeKind}`、`risk::{Risk, ToolCommand}`、`safety::forbidden::is_protected_tree_rel`、`context::Context`
- Produces:
  - `rules::Category { DevTools, AppCache, LargeFiles }`（`Copy + Eq + Ord`，可 `as usize`），`Category::ALL: [Category; 3]`、`label(self) -> &'static str`
  - `rules::Suggestion { pub node: NodeId, pub rule_id: &'static str, pub category: Category, pub risk: Risk, pub description: &'static str, pub command: Option<ToolCommand>, pub size: u64 }`
  - `rules::evaluate(tree: &Tree, ctx: &Context, now: i64) -> Vec<Suggestion>`（依 size 由大到小排序；不包含根節點；不進入受保護的子樹；比對到的節點不再往下比對）
  - `rules::CATALOG: &[Rule]`

- [ ] **Step 1: 寫失敗的測試**

`src/rules/mod.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::testutil::{find, tree_from};
    use std::ffi::OsString;
    use std::path::PathBuf;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn ctx() -> Context {
        Context { home: PathBuf::from("/h"), path_var: OsString::new() }
    }

    fn eval(t: &Tree) -> Vec<Suggestion> {
        evaluate(t, &ctx(), NOW)
    }

    fn found(t: &Tree, s: &[Suggestion]) -> Vec<(String, &'static str)> {
        let mut v: Vec<(String, &'static str)> = s
            .iter()
            .map(|x| (t.path_of(x.node).strip_prefix("/h").unwrap().to_string_lossy().into_owned(), x.rule_id))
            .collect();
        v.sort();
        v
    }

    fn pairs(items: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
        let mut v: Vec<(String, &'static str)> = items.iter().map(|(p, r)| (p.to_string(), *r)).collect();
        v.sort();
        v
    }

    #[test]
    fn xcode_derived_data_children_are_matched_not_descended() {
        let t = tree_from("/h", &[("Library/Developer/Xcode/DerivedData/App-abc/Build/x", 10)]);
        assert_eq!(
            found(&t, &eval(&t)),
            pairs(&[("Library/Developer/Xcode/DerivedData/App-abc", "xcode-derived-data")])
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
        assert_eq!(found(&t, &eval(&t)), pairs(&[("Projects/web/node_modules", "node-modules")]));
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
                (".nvm/versions/node/v24/lib/node_modules/pnpm/package.json", 1),
                (".nvm/versions/node/v24/lib/node_modules/pnpm/node_modules/a/i.js", 1),
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
        let t = tree_from("/h", &[("Projects/cli/Cargo.toml", 1), ("Projects/cli/target/debug/cli", 100)]);
        assert_eq!(found(&t, &eval(&t)), pairs(&[("Projects/cli/target", "rust-target")]));
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
            &[("Library/Logs/DiagnosticReports/a", 1), ("Library/Logs/disk-sweeper/actions.jsonl", 1)],
        );
        assert_eq!(found(&t, &eval(&t)), pairs(&[("Library/Logs/DiagnosticReports", "user-logs")]));
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
            pairs(&[("Downloads/中文 安裝檔.dmg", "installers"), ("Downloads/old-video.mov", "old-downloads")])
        );
    }

    #[test]
    fn large_files_are_found_outside_protected_trees() {
        let t = tree_from(
            "/h",
            &[("Movies/raw.mov", 2 << 30), ("Library/Mobile Documents/com~apple~CloudDocs/big.zip", 2 << 30)],
        );
        assert_eq!(found(&t, &eval(&t)), pairs(&[("Movies/raw.mov", "large-files")]));
    }

    #[test]
    fn docker_is_tool_managed_and_not_descended() {
        let t = tree_from("/h", &[("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw", 4 << 30)]);
        assert_eq!(found(&t, &eval(&t)), pairs(&[("Library/Containers/com.docker.docker", "docker")]));
    }

    #[test]
    fn scan_root_below_home_uses_home_relative_paths() {
        let t = tree_from("/h/Projects", &[("web/package.json", 1), ("web/node_modules/a", 1)]);
        assert_eq!(found(&t, &eval(&t)), pairs(&[("Projects/web/node_modules", "node-modules")]));
    }

    #[test]
    fn scan_root_outside_home_yields_nothing() {
        let t = tree_from("/other", &[("web/package.json", 1), ("web/node_modules/a", 1)]);
        assert!(eval(&t).is_empty());
    }

    #[test]
    fn suggestions_are_sorted_by_size_desc() {
        let t = tree_from("/h", &[(".cache/small/x", 1), (".cache/big/x", 100)]);
        let s = eval(&t);
        assert_eq!(s.iter().map(|x| x.size).collect::<Vec<_>>(), [100, 1]);
    }
}
```

- [ ] **Step 2: 確認測試失敗**

`src/lib.rs` 加入 `pub mod rules;`。

Run: `cargo test --lib rules`
Expected: 編譯失敗（`evaluate`、`Suggestion` 未定義）

- [ ] **Step 3: 實作 `src/rules/mod.rs`**（加在測試模組上方）

```rust
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
    ChildOf { parents: &'static [&'static str], exclude: &'static [&'static str] },
    /// 某目錄下、名稱以 suffix 結尾的直接子項目
    ChildWithSuffix { parent: &'static str, suffix: &'static str },
    /// 名為 name 的目錄，同層有 sibling；路徑中不可有 `.` 開頭的目錄、`Library` 或另一個 name
    DirWithSibling { name: &'static str, sibling: &'static str },
    /// under 之下任何深度、副檔名符合的檔案
    ExtUnder { under: &'static str, exts: &'static [&'static str] },
    /// parent 的直接子項目，超過指定天數未修改且夠大
    OldChild { parent: &'static str, min_age_days: i64, min_size: u64 },
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
            Matcher::ChildOf { parents, exclude } => parents.contains(&parent_rel) && !exclude.contains(&name),
            Matcher::ChildWithSuffix { parent, suffix } => parent_rel == parent && name.ends_with(suffix),
            Matcher::DirWithSibling { name: want, sibling } => {
                node.kind == NodeKind::Dir
                    && name == want
                    && !parent_rel.split('/').any(|c| c == want || c.starts_with('.') || c == "Library")
                    && node.parent.and_then(|p| tree.child_named(p, sibling)).is_some()
            }
            Matcher::ExtUnder { under, exts } => {
                node.kind == NodeKind::File
                    && rel.starts_with(under)
                    && rel.as_bytes().get(under.len()) == Some(&b'/')
                    && name.rsplit_once('.').is_some_and(|(_, ext)| exts.iter().any(|e| e.eq_ignore_ascii_case(ext)))
            }
            Matcher::OldChild { parent, min_age_days, min_size } => {
                parent_rel == parent && node.size >= min_size && now - node.mtime > min_age_days * 86_400
            }
            Matcher::LargeFile { min_size } => node.kind == NodeKind::File && node.size >= min_size,
        }
    }
}

/// 單次走訪樹：每個節點依 CATALOG 順序比對，第一個符合者勝出，且不再往下比對。
pub fn evaluate(tree: &Tree, ctx: &Context, now: i64) -> Vec<Suggestion> {
    let Ok(root_rel) = tree.root_path().strip_prefix(&ctx.home) else { return Vec::new() };
    let mut out = Vec::new();
    let mut stack = vec![(tree.root(), root_rel.to_string_lossy().into_owned())];
    while let Some((id, rel)) = stack.pop() {
        if forbidden::is_protected_tree_rel(&rel) {
            continue;
        }
        if id != tree.root() {
            if let Some(rule) = CATALOG.iter().find(|r| r.matcher.matches(tree, id, &rel, now)) {
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
            let child_rel = if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
            stack.push((child, child_rel));
        }
    }
    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}
```

- [ ] **Step 4: 實作 `src/rules/catalog.rs`**

```rust
use super::Category::{AppCache, DevTools, LargeFiles};
use super::Matcher::{ChildOf, ChildWithSuffix, DirWithSibling, Exact, ExtUnder, LargeFile, OldChild};
use super::Rule;
use crate::risk::Risk::{Regenerable, ToolManaged, UserData};
use crate::risk::ToolCommand;

const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;
const NONE: &[&str] = &[];

/// 依序比對，第一條符合者勝出：具體規則必須排在通用規則之前。
pub static CATALOG: &[Rule] = &[
    // ── 開發工具 ──
    Rule {
        id: "xcode-derived-data",
        category: DevTools,
        risk: Regenerable,
        matcher: ChildOf { parents: &["Library/Developer/Xcode/DerivedData"], exclude: NONE },
        command: None,
        description: "Xcode 的 build 中間產物，下次 build 會自動重建（第一次 build 會較慢）",
    },
    Rule {
        id: "xcode-archives",
        category: DevTools,
        risk: UserData,
        matcher: ChildOf { parents: &["Library/Developer/Xcode/Archives"], exclude: NONE },
        command: None,
        description: "Xcode Archive（含已發佈版本的 dSYM）；若需要追查舊版 crash 請保留",
    },
    Rule {
        id: "xcode-device-support",
        category: DevTools,
        risk: Regenerable,
        matcher: ChildOf {
            parents: &[
                "Library/Developer/Xcode/iOS DeviceSupport",
                "Library/Developer/Xcode/watchOS DeviceSupport",
                "Library/Developer/Xcode/tvOS DeviceSupport",
            ],
            exclude: NONE,
        },
        command: None,
        description: "連接實機時產生的符號檔，下次連接該裝置時會重新產生",
    },
    Rule {
        id: "simulator",
        category: DevTools,
        risk: ToolManaged,
        matcher: Exact(&["Library/Developer/CoreSimulator"]),
        command: Some(ToolCommand { program: "xcrun", args: &["simctl", "delete", "unavailable"] }),
        description: "iOS Simulator 的裝置與資料；官方指令只會刪除目前 Xcode 已不支援的 simulator",
    },
    Rule {
        id: "node-modules",
        category: DevTools,
        risk: Regenerable,
        matcher: DirWithSibling { name: "node_modules", sibling: "package.json" },
        command: None,
        description: "專案的 npm 套件，執行 npm / pnpm / bun install 即可重建",
    },
    Rule {
        id: "rust-target",
        category: DevTools,
        risk: Regenerable,
        matcher: DirWithSibling { name: "target", sibling: "Cargo.toml" },
        command: None,
        description: "Rust 的 build 產物，cargo build 會重建",
    },
    Rule {
        id: "npm-cache",
        category: DevTools,
        risk: Regenerable,
        matcher: Exact(&[".npm/_cacache"]),
        command: None,
        description: "npm 下載快取，需要時會重新下載",
    },
    Rule {
        id: "bun-cache",
        category: DevTools,
        risk: Regenerable,
        matcher: Exact(&[".bun/install/cache"]),
        command: None,
        description: "bun 下載快取，需要時會重新下載",
    },
    Rule {
        id: "pnpm-store",
        category: DevTools,
        risk: ToolManaged,
        matcher: Exact(&["Library/pnpm/store"]),
        command: Some(ToolCommand { program: "pnpm", args: &["store", "prune"] }),
        description: "pnpm 共用套件庫；直接刪除會破壞既有專案的硬連結，請用官方指令清理未使用的套件",
    },
    Rule {
        id: "yarn-cache",
        category: DevTools,
        risk: Regenerable,
        matcher: Exact(&["Library/Caches/Yarn"]),
        command: None,
        description: "Yarn 下載快取，需要時會重新下載",
    },
    Rule {
        id: "cargo-registry-cache",
        category: DevTools,
        risk: Regenerable,
        matcher: Exact(&[".cargo/registry/cache", ".cargo/registry/src"]),
        command: None,
        description: "Cargo 下載的 crate 快取與解壓縮原始碼，需要時會重新下載",
    },
    Rule {
        id: "gradle-cache",
        category: DevTools,
        risk: Regenerable,
        matcher: Exact(&[".gradle/caches"]),
        command: None,
        description: "Gradle 依賴與 build 快取，下次 build 會重新下載",
    },
    Rule {
        id: "pip-cache",
        category: DevTools,
        risk: Regenerable,
        matcher: Exact(&["Library/Caches/pip"]),
        command: None,
        description: "pip 下載快取，需要時會重新下載",
    },
    Rule {
        id: "xdg-cache",
        category: DevTools,
        risk: Regenerable,
        matcher: ChildOf { parents: &[".cache"], exclude: NONE },
        command: None,
        description: "命令列工具的快取目錄（~/.cache），工具會自動重建",
    },
    Rule {
        id: "docker",
        category: DevTools,
        risk: ToolManaged,
        matcher: Exact(&["Library/Containers/com.docker.docker"]),
        command: Some(ToolCommand { program: "docker", args: &["system", "prune", "-f"] }),
        description: "Docker Desktop 的 VM 磁碟映像；直接刪檔會失去所有 image 與 volume，請用官方指令清理",
    },
    Rule {
        id: "orbstack",
        category: DevTools,
        risk: ToolManaged,
        matcher: Exact(&[".orbstack"]),
        command: None,
        description: "OrbStack 的資料；請在 OrbStack 內清理 image 與 container",
    },
    Rule {
        id: "orbstack",
        category: DevTools,
        risk: ToolManaged,
        matcher: ChildWithSuffix { parent: "Library/Group Containers", suffix: ".dev.orbstack" },
        command: None,
        description: "OrbStack 的資料；請在 OrbStack 內清理 image 與 container",
    },
    // ── 系統與 App cache ──
    Rule {
        id: "homebrew-cache",
        category: AppCache,
        risk: ToolManaged,
        matcher: Exact(&["Library/Caches/Homebrew"]),
        command: Some(ToolCommand { program: "brew", args: &["cleanup", "--prune=all"] }),
        description: "Homebrew 下載的安裝檔快取",
    },
    Rule {
        id: "app-caches",
        category: AppCache,
        risk: Regenerable,
        matcher: ChildOf { parents: &["Library/Caches"], exclude: NONE },
        command: None,
        description: "App 的快取，App 會自動重建；若 App 正在執行，少數情況需要重開",
    },
    Rule {
        id: "user-logs",
        category: AppCache,
        risk: Regenerable,
        matcher: ChildOf { parents: &["Library/Logs"], exclude: &["disk-sweeper"] },
        command: None,
        description: "App 與系統的 log 檔",
    },
    // ── 大型使用者檔案 ──
    Rule {
        id: "installers",
        category: LargeFiles,
        risk: UserData,
        matcher: ExtUnder { under: "Downloads", exts: &["dmg", "pkg", "iso"] },
        command: None,
        description: "下載的安裝檔，安裝完成後通常不需要保留",
    },
    Rule {
        id: "old-downloads",
        category: LargeFiles,
        risk: UserData,
        matcher: OldChild { parent: "Downloads", min_age_days: 90, min_size: 50 * MB },
        command: None,
        description: "下載資料夾中超過 90 天未修改的大型項目",
    },
    Rule {
        id: "ios-backups",
        category: LargeFiles,
        risk: UserData,
        matcher: ChildOf { parents: &["Library/Application Support/MobileSync/Backup"], exclude: NONE },
        command: None,
        description: "iPhone / iPad 的本機備份；確認已有 iCloud 或較新的備份再刪除",
    },
    Rule {
        id: "large-files",
        category: LargeFiles,
        risk: UserData,
        matcher: LargeFile { min_size: GB },
        command: None,
        description: "大於 1 GB 的檔案",
    },
];
```

- [ ] **Step 5: 確認測試通過**

Run: `cargo test --lib rules`
Expected: 13 passed

- [ ] **Step 6: Commit**

```bash
git add src/rules src/lib.rs
git commit -m "feat: add cleanup rule engine and catalog"
```

---

### Task 6: 執行清理、操作紀錄、執行中 App 偵測

**Files:**
- Create: `src/action/mod.rs`, `src/action/journal.rs`, `src/action/procs.rs`
- Modify: `src/lib.rs`（加入 `pub mod action;`）

**Interfaces:**
- Consumes: `safety::{ApprovedTarget, review, Candidate, Verdict}`、`risk::{Handling, Risk, ToolCommand}`、`context::Context`
- Produces:
  - `action::Trasher` trait（`fn trash(&self, path: &Path) -> Result<(), String>`）、`action::SystemTrash`
  - `action::Outcome { Done, DryRun, Skipped(String), Failed(String) }`（`Clone + Eq`）
  - `action::ActionResult { pub path: PathBuf, pub size: u64, pub handling: Handling, pub outcome: Outcome, pub output: Option<String> }`
  - `action::execute(targets: &[ApprovedTarget], dry_run: bool, log: Option<&mut ActionLog>, trasher: &dyn Trasher) -> Vec<ActionResult>`（結果順序與 targets 相同）
  - `action::journal::{ActionLog, ActionRecord}`：`ActionLog::open(dir: &Path) -> io::Result<ActionLog>`、`write(&mut self, &ActionRecord) -> io::Result<()>`
  - `action::procs::{apps_for_rules, is_running, running_app_warnings}`：`running_app_warnings<'a>(rule_ids: impl IntoIterator<Item = &'a str>) -> Vec<String>`

- [ ] **Step 1: 寫失敗的測試**

`src/action/procs.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_rules_to_apps_without_duplicates() {
        let apps = apps_for_rules(["xcode-derived-data", "xcode-archives", "docker", "npm-cache"]);
        assert_eq!(apps, vec![("Xcode", "Xcode"), ("Docker Desktop", "com.docker.backend")]);
    }

    #[test]
    fn unknown_process_is_not_running() {
        assert!(!is_running("disk-sweeper-no-such-process-xyz"));
    }
}
```

`src/action/mod.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::risk::{Risk, ToolCommand};
    use crate::safety::{review, Candidate, Verdict};
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;

    #[derive(Default)]
    struct FakeTrash(RefCell<Vec<PathBuf>>);

    impl Trasher for FakeTrash {
        fn trash(&self, path: &Path) -> Result<(), String> {
            self.0.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
    }

    fn setup() -> (TempDir, Context) {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("Projects")).unwrap();
        let ctx = Context::new(d.path(), OsString::from("/usr/bin:/bin")).unwrap();
        (d, ctx)
    }

    fn approve(path: &Path, risk: Risk, rule: Option<&'static str>, command: Option<ToolCommand>, ctx: &Context) -> ApprovedTarget {
        let m = fs::symlink_metadata(path).unwrap();
        let c = Candidate {
            path: path.to_path_buf(),
            risk,
            rule_id: rule,
            command,
            size: m.blocks() * 512,
            inode: m.ino(),
            mtime: m.mtime(),
        };
        match review(c, ctx) {
            Verdict::Approved(t) => t,
            Verdict::Rejected { reason, .. } => panic!("{reason}"),
        }
    }

    fn log_in(d: &TempDir) -> ActionLog {
        ActionLog::open(&d.path().join("logs")).unwrap()
    }

    #[test]
    fn deletes_regenerable_dir_and_writes_log() {
        let (d, ctx) = setup();
        let target = ctx.home.join("Projects/web/node_modules");
        fs::create_dir_all(target.join("a")).unwrap();
        fs::write(target.join("a/i.js"), "x").unwrap();
        let t = approve(&target, Risk::Regenerable, Some("node-modules"), None, &ctx);
        let mut log = log_in(&d);
        let res = execute(&[t], false, Some(&mut log), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Done);
        assert!(!target.exists());
        let text = fs::read_to_string(d.path().join("logs/actions.jsonl")).unwrap();
        let rec: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(rec["handling"], "delete");
        assert_eq!(rec["result"], "ok");
        assert_eq!(rec["rule"], "node-modules");
        assert_eq!(rec["dry_run"], false);
    }

    #[test]
    fn user_data_goes_through_trasher() {
        let (d, ctx) = setup();
        let f = ctx.home.join("Projects/notes.txt");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::UserData, None, None, &ctx);
        let trash = FakeTrash::default();
        let res = execute(&[t], false, Some(&mut log_in(&d)), &trash);
        assert_eq!(res[0].outcome, Outcome::Done);
        assert_eq!(trash.0.borrow().as_slice(), [f.clone()]);
        assert!(f.exists(), "fake trash must not touch the file");
    }

    #[test]
    fn changed_after_scan_is_skipped() {
        let (d, ctx) = setup();
        let f = ctx.home.join("Projects/cache.bin");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        fs::remove_file(&f).unwrap();
        fs::write(&f, "new content").unwrap(); // 新檔案：inode 不同
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Skipped("掃描後已變動".into()));
        assert!(f.exists());
    }

    #[test]
    fn vanished_path_is_skipped() {
        let (d, ctx) = setup();
        let f = ctx.home.join("Projects/gone.bin");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        fs::remove_file(&f).unwrap();
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Skipped("已不存在".into()));
    }

    #[test]
    fn dry_run_changes_nothing_and_needs_no_log() {
        let (_d, ctx) = setup();
        let f = ctx.home.join("Projects/cache.bin");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        let res = execute(&[t], true, None, &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::DryRun);
        assert!(f.exists());
    }

    #[test]
    fn refuses_without_log_when_not_dry_run() {
        let (_d, ctx) = setup();
        let f = ctx.home.join("Projects/cache.bin");
        fs::write(&f, "x").unwrap();
        let t = approve(&f, Risk::Regenerable, Some("r"), None, &ctx);
        let res = execute(&[t], false, None, &FakeTrash::default());
        assert!(matches!(res[0].outcome, Outcome::Failed(_)));
        assert!(f.exists());
    }

    #[test]
    fn deleting_symlink_keeps_target() {
        let (d, ctx) = setup();
        let real = ctx.home.join("Projects/real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("keep.txt"), "x").unwrap();
        let link = ctx.home.join("Projects/link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let t = approve(&link, Risk::Regenerable, Some("r"), None, &ctx);
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Done);
        assert!(fs::symlink_metadata(&link).is_err());
        assert!(real.join("keep.txt").exists());
    }

    #[test]
    fn runs_tool_command_and_captures_output() {
        let (d, ctx) = setup();
        let dir = ctx.home.join("Projects/tool");
        fs::create_dir_all(&dir).unwrap();
        let cmd = ToolCommand { program: "echo", args: &["hello"] };
        let t = approve(&dir, Risk::ToolManaged, Some("t"), Some(cmd), &ctx);
        let res = execute(&[t], false, Some(&mut log_in(&d)), &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::Done);
        assert!(res[0].output.as_deref().unwrap().contains("hello"));
        assert!(dir.exists());
    }

    #[test]
    fn tool_command_in_dry_run_is_not_executed() {
        let (_d, ctx) = setup();
        let dir = ctx.home.join("Projects/tool");
        fs::create_dir_all(&dir).unwrap();
        let cmd = ToolCommand { program: "echo", args: &["hello"] };
        let t = approve(&dir, Risk::ToolManaged, Some("t"), Some(cmd), &ctx);
        let res = execute(&[t], true, None, &FakeTrash::default());
        assert_eq!(res[0].outcome, Outcome::DryRun);
        assert_eq!(res[0].output.as_deref(), Some("將執行：echo hello"));
    }
}
```

- [ ] **Step 2: 確認測試失敗**

`src/lib.rs` 加入 `pub mod action;`。

Run: `cargo test --lib action`
Expected: 編譯失敗（`execute`、`ActionLog`、`apps_for_rules` 未定義）

- [ ] **Step 3: 實作 `src/action/journal.rs`**

```rust
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;

/// 操作紀錄（JSONL，一個目標一行，只會附加）。
pub struct ActionLog {
    file: File,
}

impl ActionLog {
    pub fn open(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new().create(true).append(true).open(dir.join("actions.jsonl"))?;
        Ok(Self { file })
    }

    pub fn write(&mut self, rec: &ActionRecord) -> io::Result<()> {
        let line = serde_json::to_string(rec).map_err(io::Error::other)?;
        writeln!(self.file, "{line}")
    }
}

#[derive(Debug, Serialize)]
pub struct ActionRecord {
    pub ts: String,
    pub path: String,
    pub size: u64,
    pub handling: &'static str,
    pub rule: Option<&'static str>,
    pub result: &'static str,
    pub detail: String,
    pub dry_run: bool,
}
```

- [ ] **Step 4: 實作 `src/action/procs.rs`**（加在測試模組上方）

```rust
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
pub fn apps_for_rules<'a>(rule_ids: impl IntoIterator<Item = &'a str>) -> Vec<(&'static str, &'static str)> {
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
```

- [ ] **Step 5: 實作 `src/action/mod.rs`**（加在測試模組上方）

```rust
pub mod journal;
pub mod procs;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::risk::Handling;
use crate::safety::ApprovedTarget;
use journal::{ActionLog, ActionRecord};

const MAX_DETAIL_CHARS: usize = 2000;

pub trait Trasher {
    fn trash(&self, path: &Path) -> Result<(), String>;
}

/// macOS 垃圾桶（trash crate 預設走 Finder，支援「放回原處」）
pub struct SystemTrash;

impl Trasher for SystemTrash {
    fn trash(&self, path: &Path) -> Result<(), String> {
        trash::delete(path).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    DryRun,
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ActionResult {
    pub path: PathBuf,
    pub size: u64,
    pub handling: Handling,
    pub outcome: Outcome,
    pub output: Option<String>,
}

/// 依序處理每個目標；單項失敗不影響其他項。非 dry-run 時沒有紀錄檔就全部拒絕。
pub fn execute(
    targets: &[ApprovedTarget],
    dry_run: bool,
    mut log: Option<&mut ActionLog>,
    trasher: &dyn Trasher,
) -> Vec<ActionResult> {
    let mut results = Vec::with_capacity(targets.len());
    for t in targets {
        let (outcome, output) = if !dry_run && log.is_none() {
            (Outcome::Failed("無法寫入操作紀錄，拒絕執行".into()), None)
        } else {
            run_one(t, dry_run, trasher)
        };
        let mut result =
            ActionResult { path: t.path().to_path_buf(), size: t.size(), handling: t.handling(), outcome, output };
        if let Some(log) = log.as_deref_mut() {
            if let Err(e) = log.write(&record_for(t, &result, dry_run)) {
                let note = format!("（操作紀錄寫入失敗：{e}）");
                result.output = Some(match result.output.take() {
                    Some(o) => format!("{o}\n{note}"),
                    None => note,
                });
            }
        }
        results.push(result);
    }
    results
}

fn run_one(t: &ApprovedTarget, dry_run: bool, trasher: &dyn Trasher) -> (Outcome, Option<String>) {
    if let Handling::Command(cmd) = t.handling() {
        if dry_run {
            return (Outcome::DryRun, Some(format!("將執行：{}", cmd.display())));
        }
        return match Command::new(cmd.program).args(cmd.args).output() {
            Ok(out) => {
                let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
                if out.status.success() {
                    (Outcome::Done, Some(text))
                } else {
                    (Outcome::Failed(format!("指令結束狀態：{}", out.status)), Some(text))
                }
            }
            Err(e) => (Outcome::Failed(e.to_string()), None),
        };
    }

    // 執行前重新 lstat：掃描後被改過或已消失就略過
    let meta = match std::fs::symlink_metadata(t.path()) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Outcome::Skipped("已不存在".into()), None),
        Err(e) => return (Outcome::Failed(e.to_string()), None),
    };
    if meta.ino() != t.inode() || meta.mtime() != t.mtime() {
        return (Outcome::Skipped("掃描後已變動".into()), None);
    }
    if dry_run {
        return (Outcome::DryRun, None);
    }
    let res = match t.handling() {
        // trash 失敗時只回報錯誤，絕不改為直接刪除
        Handling::Trash => trasher.trash(t.path()),
        // symlink_metadata 對 symlink 回傳 is_dir() == false，所以只會刪除連結本身
        Handling::Delete if meta.is_dir() => std::fs::remove_dir_all(t.path()).map_err(|e| e.to_string()),
        Handling::Delete => std::fs::remove_file(t.path()).map_err(|e| e.to_string()),
        Handling::Command(_) => unreachable!("handled above"),
    };
    match res {
        Ok(()) => (Outcome::Done, None),
        Err(e) => (Outcome::Failed(e), None),
    }
}

fn record_for(t: &ApprovedTarget, r: &ActionResult, dry_run: bool) -> ActionRecord {
    let (result, detail) = match &r.outcome {
        Outcome::Done | Outcome::DryRun => ("ok", r.output.clone().unwrap_or_default()),
        Outcome::Skipped(m) => ("skipped", m.clone()),
        Outcome::Failed(m) => ("error", m.clone()),
    };
    ActionRecord {
        ts: chrono::Local::now().to_rfc3339(),
        path: r.path.display().to_string(),
        size: r.size,
        handling: match r.handling {
            Handling::Delete => "delete",
            Handling::Trash => "trash",
            Handling::Command(_) => "command",
        },
        rule: t.rule_id(),
        result,
        detail: detail.chars().take(MAX_DETAIL_CHARS).collect(),
        dry_run,
    }
}
```

- [ ] **Step 6: 確認測試通過**

Run: `cargo test --lib action`
Expected: 11 passed

- [ ] **Step 7: Commit**

```bash
git add src/action src/lib.rs
git commit -m "feat: add cleanup executor with journal and running-app checks"
```

---

### Task 7: 磁碟資訊與 App 狀態機

**Files:**
- Create: `src/disk.rs`, `src/app.rs`
- Modify: `src/lib.rs`（加入 `pub mod app;`、`pub mod disk;`）

**Interfaces:**
- Consumes: 前面所有 task 的公開介面
- Produces:
  - `disk::DiskInfo { pub total: u64, pub free: u64 }`、`disk::disk_info(path: &Path) -> io::Result<DiskInfo>`
  - `app::Mode { Explore, Suggest }`、`app::Screen { Main, Confirm, Running, Result, Help, Errors }`
  - `app::Mark { pub risk, pub rule_id, pub command }`
  - `app::ConfirmState { pub items: Vec<(NodeId, Verdict)>, pub warnings: Vec<String>, pub need_typed: bool, pub input: String }`
  - `app::SuggestRow { Header(Category), Item(usize) }`（`Copy`）
  - `app::Effect { Execute(Vec<(NodeId, ApprovedTarget)>) }`
  - `app::App`：公開欄位 `ctx, dry_run, disk, progress, tree, errors, needs_fda, suggestions, mode, screen, explore: Vec<(NodeId, usize)>, suggest_cursor, expanded: [bool; 3], show_small, marks: BTreeMap<NodeId, Mark>, confirm, results, freed, status, quit`
  - 方法：`App::new(ctx, dry_run, disk: Option<DiskInfo>, now: i64)`、`on_scan_event(ScanEvent)`、`on_key(KeyEvent) -> Option<Effect>`、`on_executed(Vec<(NodeId, ActionResult)>, freed: Option<i64>)`、`on_execute_error(String)`、`explore_children() -> Vec<NodeId>`、`suggestion_for(NodeId) -> Option<&Suggestion>`、`suggest_rows() -> Vec<SuggestRow>`、`visible_items(Category) -> impl Iterator<Item = usize>`、`marked_total() -> u64`

- [ ] **Step 1: 寫失敗的測試**

`src/disk.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_root_volume() {
        let d = disk_info(Path::new("/")).unwrap();
        assert!(d.total > 0);
        assert!(d.free <= d.total);
    }
}
```

`src/app.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Outcome;
    use crate::scan::{ScanOptions, ScanResult};
    use crate::tree::testutil::{find, tree_from};
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Duration;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, s: &str) {
        for c in s.chars() {
            assert!(app.on_key(key(c)).is_none());
        }
    }

    /// 以 /h 為 home 的假樹（不碰檔案系統）
    fn app_with(files: &[(&str, u64)]) -> App {
        let ctx = Context { home: PathBuf::from("/h"), path_var: OsString::new() };
        let mut app = App::new(ctx, false, None, 1_800_000_000);
        let tree = tree_from("/h", files);
        app.on_scan_event(ScanEvent::Done(ScanResult { tree, errors: vec![], elapsed: Duration::ZERO }));
        app
    }

    /// 真正掃描 temp home（確認流程需要 safety::review 碰檔案系統）
    fn scanned_app(home: &Path) -> App {
        let ctx = Context::new(home, OsString::from("/usr/bin:/bin")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let result = scan::scan(&ctx.home, &ScanOptions { threads: 2, min_node_size: 0 }, &tx);
        let mut app = App::new(ctx, false, None, chrono::Local::now().timestamp());
        app.on_scan_event(ScanEvent::Done(result));
        app
    }

    fn write(p: &Path, len: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![1u8; len]).unwrap();
    }

    #[test]
    fn done_event_builds_tree_and_suggestions() {
        let app = app_with(&[("Projects/web/package.json", 1), ("Projects/web/node_modules/a/i.js", 100)]);
        assert!(app.tree.is_some());
        assert_eq!(app.suggestions.len(), 1);
        assert_eq!(app.suggestions[0].rule_id, "node-modules");
    }

    #[test]
    fn keys_are_ignored_while_scanning_except_quit() {
        let ctx = Context { home: PathBuf::from("/h"), path_var: OsString::new() };
        let mut app = App::new(ctx, false, None, 0);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.mode, Mode::Explore);
        app.on_key(key('q'));
        assert!(app.quit);
    }

    #[test]
    fn explore_navigation_enters_backs_and_clamps() {
        let mut app = app_with(&[("a/b/x", 100), ("c", 1)]);
        let tree = app.tree.as_ref().unwrap();
        let (a, c) = (find(tree, "a"), find(tree, "c"));
        assert_eq!(app.explore_children(), vec![a, c]);
        app.on_key(key('j'));
        app.on_key(key('j'));
        assert_eq!(app.explore.last().unwrap().1, 1);
        app.on_key(key('k'));
        app.on_key(key('l'));
        assert_eq!(app.explore.last().unwrap().0, a);
        app.on_key(key('h'));
        assert_eq!(app.explore.len(), 1);
        app.on_key(key('h'));
        assert_eq!(app.explore.len(), 1);
    }

    #[test]
    fn marking_parent_and_child_does_not_double_count() {
        // 先標記父層，再標記子項目 → 子項目被拒絕
        let mut app = app_with(&[("a/b/x", 100), ("a/y", 10)]);
        app.on_key(key(' ')); // 標記 a
        app.on_key(key('l')); // 進入 a
        app.on_key(key(' ')); // 嘗試標記 a/b
        assert_eq!(app.marks.len(), 1);
        assert_eq!(app.marked_total(), 110);
        assert!(app.status.is_some());

        // 先標記子項目，再標記父層 → 子項目被取代
        let mut app = app_with(&[("a/b/x", 100), ("a/y", 10)]);
        app.on_key(key('l'));
        app.on_key(key(' ')); // 標記 a/b
        app.on_key(key('h'));
        app.on_key(key(' ')); // 標記 a
        let a = find(app.tree.as_ref().unwrap(), "a");
        assert_eq!(app.marks.keys().copied().collect::<Vec<_>>(), vec![a]);
        assert_eq!(app.marked_total(), 110);
    }

    #[test]
    fn protected_dirs_cannot_be_marked() {
        let mut app = app_with(&[("Library/Caches/x", 1)]);
        app.on_key(key(' '));
        assert!(app.marks.is_empty());
        assert!(app.status.as_deref().unwrap().contains("無法標記"));
    }

    #[test]
    fn suggest_header_toggles_whole_group() {
        let mut app = app_with(&[
            ("Projects/a/package.json", 1),
            ("Projects/a/node_modules/x", 20 << 20),
            ("Projects/b/package.json", 1),
            ("Projects/b/node_modules/x", 20 << 20),
        ]);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.suggest_rows()[0], SuggestRow::Header(Category::DevTools));
        app.on_key(key(' '));
        assert_eq!(app.marks.len(), 2);
        app.on_key(key(' '));
        assert!(app.marks.is_empty());
    }

    #[test]
    fn small_items_are_hidden_until_toggled() {
        let mut app = app_with(&[("Projects/a/package.json", 1), ("Projects/a/node_modules/x", 1)]);
        app.on_key(code(KeyCode::Tab));
        assert_eq!(app.visible_items(Category::DevTools).count(), 0);
        app.on_key(key('a'));
        assert_eq!(app.visible_items(Category::DevTools).count(), 1);
    }

    #[test]
    fn d_without_marks_shows_status() {
        let mut app = app_with(&[("a", 1)]);
        app.on_key(key('d'));
        assert_eq!(app.screen, Screen::Main);
        assert!(app.status.is_some());
    }

    #[test]
    fn trash_items_require_typed_yes() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("Projects/notes.txt"), 10);
        let mut app = scanned_app(home.path());
        app.on_key(key('l')); // 進入 Projects
        app.on_key(key(' ')); // 標記 notes.txt（使用者資料 → 垃圾桶）
        app.on_key(key('d'));
        assert_eq!(app.screen, Screen::Confirm);
        assert!(app.confirm.as_ref().unwrap().need_typed);
        assert!(app.on_key(key('y')).is_none()); // 單按 y 不夠
        assert!(app.on_key(code(KeyCode::Enter)).is_none());
        assert_eq!(app.screen, Screen::Confirm);
        typed(&mut app, "yes");
        let Some(Effect::Execute(targets)) = app.on_key(code(KeyCode::Enter)) else { panic!("no effect") };
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].1.handling(), Handling::Trash);
        assert_eq!(app.screen, Screen::Running);
    }

    #[test]
    fn regenerable_only_accepts_y_and_results_update_tree() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("Projects/web/package.json"), 10);
        write(&home.path().join("Projects/web/node_modules/a/i.js"), 5000);
        let mut app = scanned_app(home.path());
        let root_before = app.tree.as_ref().unwrap().node(app.tree.as_ref().unwrap().root()).size;
        app.on_key(code(KeyCode::Tab));
        app.on_key(key('a')); // 顯示小項目
        app.on_key(key('j')); // 移到 node_modules
        app.on_key(key(' '));
        app.on_key(key('d'));
        assert!(!app.confirm.as_ref().unwrap().need_typed);
        let Some(Effect::Execute(targets)) = app.on_key(key('y')) else { panic!("no effect") };
        let (id, target) = &targets[0];
        let removed = app.tree.as_ref().unwrap().node(*id).size;
        let result = ActionResult {
            path: target.path().to_path_buf(),
            size: target.size(),
            handling: target.handling(),
            outcome: Outcome::Done,
            output: None,
        };
        app.on_executed(vec![(*id, result)], Some(4096));
        let tree = app.tree.as_ref().unwrap();
        assert_eq!(tree.node(tree.root()).size, root_before - removed);
        assert!(app.suggestions.is_empty());
        assert!(app.marks.is_empty());
        assert_eq!(app.screen, Screen::Result);
        app.on_key(key('x'));
        assert_eq!(app.screen, Screen::Main);
        assert!(app.results.is_empty());
    }

    #[test]
    fn esc_cancels_confirm() {
        let home = tempfile::tempdir().unwrap();
        write(&home.path().join("Projects/notes.txt"), 10);
        let mut app = scanned_app(home.path());
        app.on_key(key('l'));
        app.on_key(key(' '));
        app.on_key(key('d'));
        app.on_key(code(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Main);
        assert!(app.confirm.is_none());
        assert_eq!(app.marks.len(), 1);
    }
}
```

- [ ] **Step 2: 確認測試失敗**

`src/lib.rs` 加入 `pub mod app;`、`pub mod disk;`。

Run: `cargo test --lib -- app disk`
Expected: 編譯失敗（`App`、`disk_info` 未定義）

- [ ] **Step 3: 實作 `src/disk.rs`**（加在測試模組上方）

```rust
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskInfo {
    pub total: u64,
    pub free: u64,
}

pub fn disk_info(path: &Path) -> io::Result<DiskInfo> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: statfs 只寫入我們提供、大小正確的結構；c 在呼叫期間有效
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let bsize = s.f_bsize as u64;
    Ok(DiskInfo { total: s.f_blocks as u64 * bsize, free: s.f_bavail as u64 * bsize })
}
```

- [ ] **Step 4: 實作 `src/app.rs`**（加在測試模組上方）

```rust
use std::collections::{BTreeMap, HashMap};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::action::{procs, ActionResult, Outcome};
use crate::context::Context;
use crate::disk::DiskInfo;
use crate::risk::{Handling, Risk, ToolCommand};
use crate::rules::{self, Category, Suggestion};
use crate::safety::{self, ApprovedTarget, Candidate, Verdict};
use crate::scan::{self, Progress, ScanError, ScanEvent};
use crate::tree::{NodeId, Tree};

const TYPED_CONFIRM_THRESHOLD: u64 = 10 << 30;
const SMALL_ITEM: u64 = 10 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Explore,
    Suggest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Main,
    Confirm,
    Running,
    Result,
    Help,
    Errors,
}

#[derive(Debug, Clone, Copy)]
pub struct Mark {
    pub risk: Risk,
    pub rule_id: Option<&'static str>,
    pub command: Option<ToolCommand>,
}

#[derive(Debug)]
pub struct ConfirmState {
    pub items: Vec<(NodeId, Verdict)>,
    pub warnings: Vec<String>,
    pub need_typed: bool,
    pub input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestRow {
    Header(Category),
    Item(usize),
}

pub enum Effect {
    Execute(Vec<(NodeId, ApprovedTarget)>),
}

pub struct App {
    pub ctx: Context,
    pub dry_run: bool,
    pub disk: Option<DiskInfo>,
    pub progress: Progress,
    pub tree: Option<Tree>,
    pub errors: Vec<ScanError>,
    pub needs_fda: bool,
    pub suggestions: Vec<Suggestion>,
    sugg_by_node: HashMap<NodeId, usize>,
    pub mode: Mode,
    pub screen: Screen,
    /// 目錄堆疊：(目錄, 游標)，最後一個是目前所在目錄
    pub explore: Vec<(NodeId, usize)>,
    pub suggest_cursor: usize,
    pub expanded: [bool; 3],
    pub show_small: bool,
    pub marks: BTreeMap<NodeId, Mark>,
    pub confirm: Option<ConfirmState>,
    pub results: Vec<ActionResult>,
    pub freed: Option<i64>,
    pub status: Option<String>,
    pub quit: bool,
    now: i64,
}

impl App {
    pub fn new(ctx: Context, dry_run: bool, disk: Option<DiskInfo>, now: i64) -> Self {
        Self {
            ctx,
            dry_run,
            disk,
            progress: Progress::default(),
            tree: None,
            errors: Vec::new(),
            needs_fda: false,
            suggestions: Vec::new(),
            sugg_by_node: HashMap::new(),
            mode: Mode::Explore,
            screen: Screen::Main,
            explore: Vec::new(),
            suggest_cursor: 0,
            expanded: [true; 3],
            show_small: false,
            marks: BTreeMap::new(),
            confirm: None,
            results: Vec::new(),
            freed: None,
            status: None,
            quit: false,
            now,
        }
    }

    pub fn on_scan_event(&mut self, ev: ScanEvent) {
        match ev {
            ScanEvent::Progress(p) => self.progress = p,
            ScanEvent::Done(r) => {
                self.needs_fda = scan::needs_full_disk_access(&r.errors, &self.ctx.home);
                self.errors = r.errors;
                self.explore = vec![(r.tree.root(), 0)];
                self.status = Some(format!("掃描完成，耗時 {:.1} 秒", r.elapsed.as_secs_f64()));
                self.tree = Some(r.tree);
                self.refresh_suggestions();
            }
        }
    }

    fn refresh_suggestions(&mut self) {
        if let Some(tree) = &self.tree {
            self.suggestions = rules::evaluate(tree, &self.ctx, self.now);
            self.sugg_by_node = self.suggestions.iter().enumerate().map(|(i, s)| (s.node, i)).collect();
        }
    }

    pub fn explore_children(&self) -> Vec<NodeId> {
        match (&self.tree, self.explore.last()) {
            (Some(t), Some(&(dir, _))) => t.children_sorted_by_size(dir),
            _ => Vec::new(),
        }
    }

    pub fn suggestion_for(&self, id: NodeId) -> Option<&Suggestion> {
        self.sugg_by_node.get(&id).map(|&i| &self.suggestions[i])
    }

    pub fn visible_items(&self, cat: Category) -> impl Iterator<Item = usize> + '_ {
        self.suggestions
            .iter()
            .enumerate()
            .filter(move |(_, s)| s.category == cat && (self.show_small || s.size >= SMALL_ITEM))
            .map(|(i, _)| i)
    }

    pub fn suggest_rows(&self) -> Vec<SuggestRow> {
        let mut rows = Vec::new();
        for cat in Category::ALL {
            rows.push(SuggestRow::Header(cat));
            if self.expanded[cat as usize] {
                rows.extend(self.visible_items(cat).map(SuggestRow::Item));
            }
        }
        rows
    }

    pub fn marked_total(&self) -> u64 {
        self.tree.as_ref().map_or(0, |t| self.marks.keys().map(|&id| t.node(id).size).sum())
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return None;
        }
        self.status = None;
        match self.screen {
            Screen::Main => self.on_main_key(key),
            Screen::Confirm => self.on_confirm_key(key),
            Screen::Result => {
                self.results.clear();
                self.freed = None;
                self.screen = Screen::Main;
                None
            }
            Screen::Help | Screen::Errors => {
                self.screen = Screen::Main;
                None
            }
            Screen::Running => None,
        }
    }

    fn on_main_key(&mut self, key: KeyEvent) -> Option<Effect> {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.screen = Screen::Help,
            KeyCode::Char('u') => self.screen = Screen::Errors,
            _ if self.tree.is_none() => {}
            KeyCode::Tab => {
                self.mode = match self.mode {
                    Mode::Explore => Mode::Suggest,
                    Mode::Suggest => Mode::Explore,
                }
            }
            KeyCode::Char('d') => self.begin_confirm(),
            code => match self.mode {
                Mode::Explore => self.on_explore_key(code),
                Mode::Suggest => self.on_suggest_key(code),
            },
        }
        None
    }

    fn on_explore_key(&mut self, code: KeyCode) {
        let children = self.explore_children();
        let Some(&(_, cursor)) = self.explore.last() else { return };
        let selected = children.get(cursor).copied();
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.set_explore_cursor((cursor + 1).min(children.len().saturating_sub(1))),
            KeyCode::Up | KeyCode::Char('k') => self.set_explore_cursor(cursor.saturating_sub(1)),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => {
                if let (Some(id), Some(tree)) = (selected, &self.tree) {
                    if !tree.node(id).children.is_empty() {
                        self.explore.push((id, 0));
                    }
                }
            }
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => {
                if self.explore.len() > 1 {
                    self.explore.pop();
                }
            }
            KeyCode::Char(' ') => {
                if let Some(id) = selected {
                    self.toggle_mark(id);
                }
            }
            _ => {}
        }
    }

    fn set_explore_cursor(&mut self, cursor: usize) {
        if let Some(level) = self.explore.last_mut() {
            level.1 = cursor;
        }
    }

    fn on_suggest_key(&mut self, code: KeyCode) {
        let rows = self.suggest_rows();
        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.suggest_cursor = (self.suggest_cursor + 1).min(rows.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.suggest_cursor = self.suggest_cursor.saturating_sub(1),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => {
                if let Some(SuggestRow::Header(cat)) = rows.get(self.suggest_cursor) {
                    let i = *cat as usize;
                    self.expanded[i] = !self.expanded[i];
                    self.clamp_suggest_cursor();
                }
            }
            KeyCode::Char(' ') => match rows.get(self.suggest_cursor) {
                Some(SuggestRow::Header(cat)) => self.toggle_group(*cat),
                Some(SuggestRow::Item(i)) => {
                    let node = self.suggestions[*i].node;
                    self.toggle_mark(node);
                }
                None => {}
            },
            KeyCode::Char('a') => {
                self.show_small = !self.show_small;
                self.clamp_suggest_cursor();
            }
            _ => {}
        }
    }

    fn clamp_suggest_cursor(&mut self) {
        let n = self.suggest_rows().len();
        self.suggest_cursor = self.suggest_cursor.min(n.saturating_sub(1));
    }

    fn toggle_group(&mut self, cat: Category) {
        let nodes: Vec<NodeId> = self.visible_items(cat).map(|i| self.suggestions[i].node).collect();
        if nodes.is_empty() {
            return;
        }
        if nodes.iter().all(|n| self.marks.contains_key(n)) {
            for n in nodes {
                self.marks.remove(&n);
            }
        } else {
            for n in nodes {
                if !self.marks.contains_key(&n) {
                    self.mark(n);
                }
            }
        }
    }

    fn toggle_mark(&mut self, id: NodeId) {
        if self.marks.remove(&id).is_none() {
            self.mark(id);
        }
    }

    /// 標記一個節點：受保護的拒絕；上層已標記的拒絕；已標記的子孫會被取代，避免重複計算與重複刪除。
    fn mark(&mut self, id: NodeId) {
        let Some(tree) = &self.tree else { return };
        if id == tree.root() {
            return;
        }
        if let Some(reason) = safety::protection_reason(&tree.path_of(id), &self.ctx) {
            self.status = Some(format!("無法標記：{reason}"));
            return;
        }
        if self.marks.keys().any(|&m| tree.is_ancestor(m, id)) {
            self.status = Some("上層目錄已標記".into());
            return;
        }
        let mark = match self.sugg_by_node.get(&id) {
            Some(&i) => {
                let s = &self.suggestions[i];
                Mark { risk: s.risk, rule_id: Some(s.rule_id), command: s.command }
            }
            None => Mark { risk: Risk::UserData, rule_id: None, command: None },
        };
        self.marks.retain(|&m, _| !tree.is_ancestor(id, m));
        self.marks.insert(id, mark);
    }

    fn begin_confirm(&mut self) {
        let Some(tree) = &self.tree else { return };
        if self.marks.is_empty() {
            self.status = Some("尚未標記任何項目".into());
            return;
        }
        let items: Vec<(NodeId, Verdict)> = self
            .marks
            .iter()
            .map(|(&id, m)| {
                let n = tree.node(id);
                let c = Candidate {
                    path: tree.path_of(id),
                    risk: m.risk,
                    rule_id: m.rule_id,
                    command: m.command,
                    size: n.size,
                    inode: n.inode,
                    mtime: n.mtime,
                };
                (id, safety::review(c, &self.ctx))
            })
            .collect();
        let approved: Vec<&ApprovedTarget> = items
            .iter()
            .filter_map(|(_, v)| match v {
                Verdict::Approved(t) => Some(t),
                Verdict::Rejected { .. } => None,
            })
            .collect();
        let total: u64 = approved.iter().map(|t| t.size()).sum();
        let need_typed = total > TYPED_CONFIRM_THRESHOLD || approved.iter().any(|t| t.handling() == Handling::Trash);
        let warnings = procs::running_app_warnings(approved.iter().filter_map(|t| t.rule_id()));
        self.confirm = Some(ConfirmState { items, warnings, need_typed, input: String::new() });
        self.screen = Screen::Confirm;
    }

    fn on_confirm_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let state = self.confirm.as_mut()?;
        if key.code == KeyCode::Esc {
            self.cancel_confirm();
            return None;
        }
        if state.need_typed {
            match key.code {
                KeyCode::Char(c) => state.input.push(c),
                KeyCode::Backspace => {
                    state.input.pop();
                }
                KeyCode::Enter if state.input == "yes" => return self.confirm_execute(),
                KeyCode::Enter => {
                    state.input.clear();
                    self.status = Some("請輸入 yes 以確認".into());
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => self.confirm_execute(),
            KeyCode::Char('n') => {
                self.cancel_confirm();
                None
            }
            _ => None,
        }
    }

    fn cancel_confirm(&mut self) {
        self.confirm = None;
        self.screen = Screen::Main;
    }

    fn confirm_execute(&mut self) -> Option<Effect> {
        let state = self.confirm.take()?;
        let targets: Vec<(NodeId, ApprovedTarget)> = state
            .items
            .into_iter()
            .filter_map(|(id, v)| match v {
                Verdict::Approved(t) => Some((id, t)),
                Verdict::Rejected { .. } => None,
            })
            .collect();
        if targets.is_empty() {
            self.status = Some("沒有可執行的項目".into());
            self.screen = Screen::Main;
            return None;
        }
        self.screen = Screen::Running;
        Some(Effect::Execute(targets))
    }

    pub fn on_executed(&mut self, done: Vec<(NodeId, ActionResult)>, freed: Option<i64>) {
        if let Some(tree) = &mut self.tree {
            for (id, r) in &done {
                if r.outcome == Outcome::Done && !matches!(r.handling, Handling::Command(_)) {
                    tree.subtract(*id);
                }
            }
        }
        if let Some(tree) = &self.tree {
            self.explore = vec![(tree.root(), 0)];
        }
        self.marks.clear();
        self.suggest_cursor = 0;
        self.refresh_suggestions();
        self.results = done.into_iter().map(|(_, r)| r).collect();
        self.freed = freed;
        self.screen = Screen::Result;
    }

    pub fn on_execute_error(&mut self, msg: String) {
        self.status = Some(msg);
        self.screen = Screen::Main;
    }
}
```

- [ ] **Step 5: 確認測試通過**

Run: `cargo test --lib -- app disk`
Expected: 12 passed。若出現 borrow checker 錯誤（`on_confirm_key` 中 `state` 與 `self` 同時借用），把 `state.need_typed` 與 `state.input == "yes"` 先存成區域變數再呼叫 `self` 的方法，行為不變。

- [ ] **Step 6: Commit**

```bash
git add src/app.rs src/disk.rs src/lib.rs
git commit -m "feat: add app state machine and disk info"
```

---

### Task 8: TUI 畫面

**Files:**
- Create: `src/ui/mod.rs`, `src/ui/explore.rs`, `src/ui/suggest.rs`, `src/ui/confirm.rs`, `src/ui/result.rs`, `src/ui/overlay.rs`
- Create（由 insta 產生）: `src/ui/snapshots/*.snap`
- Modify: `src/lib.rs`（加入 `pub mod ui;`）

**Interfaces:**
- Consumes: `app::{App, Mode, Screen, SuggestRow, ConfirmState}`、`size::format_size`、`safety::{protection_reason, Verdict}`、`risk::{Risk, Handling}`、`action::{ActionResult, Outcome}`
- Produces: `ui::draw(f: &mut Frame, app: &App)`；`pub(crate)` helper：`risk_style`、`bar`、`pad`、`display_path`、`popup`

- [ ] **Step 1: 寫失敗的測試**

`src/ui/mod.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{ActionResult, Outcome};
    use crate::app::ConfirmState;
    use crate::context::Context;
    use crate::disk::DiskInfo;
    use crate::risk::Handling;
    use crate::safety::Verdict;
    use crate::scan::{Progress, ScanEvent, ScanResult};
    use crate::tree::testutil::{find, tree_from};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::Duration;

    fn base_app() -> App {
        let ctx = Context { home: PathBuf::from("/h"), path_var: OsString::new() };
        App::new(ctx, false, Some(DiskInfo { total: 228 << 30, free: 10 << 30 }), 1_800_000_000)
    }

    fn sample_app() -> App {
        let mut app = base_app();
        let tree = tree_from(
            "/h",
            &[
                ("Library/Caches/com.spotify.client/data", 3 << 30),
                ("Projects/web/package.json", 4096),
                ("Projects/web/node_modules/react/index.js", 800 << 20),
                ("Downloads/中文 安裝檔.dmg", 1 << 30),
                ("Movies/raw.mov", 2 << 30),
            ],
        );
        app.on_scan_event(ScanEvent::Done(ScanResult { tree, errors: vec![], elapsed: Duration::ZERO }));
        app
    }

    fn render(app: &App) -> Terminal<TestBackend> {
        let mut t = Terminal::new(TestBackend::new(110, 24)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        t
    }

    #[test]
    fn pad_uses_display_width() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("中文", 6), "中文  ");
        // 輸出寬度一律剛好等於 width：「中文」4 欄 + 「…」1 欄 + 補 1 個空白
        assert_eq!(pad("中文字很長", 6), "中文… ");
        assert_eq!(pad("中文字很長", 6).width(), 6);
    }

    #[test]
    fn bar_is_proportional() {
        assert_eq!(bar(5, 10, 4), "██░░");
        assert_eq!(bar(1, 0, 3), "░░░");
    }

    #[test]
    fn display_path_abbreviates_home() {
        let home = Path::new("/h");
        assert_eq!(display_path(Path::new("/h"), home), "~");
        assert_eq!(display_path(Path::new("/h/a b"), home), "~/a b");
        assert_eq!(display_path(Path::new("/etc"), home), "/etc");
    }

    #[test]
    fn scanning_screen() {
        let mut app = base_app();
        app.on_scan_event(ScanEvent::Progress(Progress {
            files: 12_345,
            bytes: 5 << 30,
            errors: 2,
            current: "/h/Library/Caches".into(),
            top: vec![("Library".into(), 3 << 30), ("Projects".into(), 1 << 30)],
        }));
        insta::assert_snapshot!(render(&app).backend());
    }

    #[test]
    fn explore_screen() {
        insta::assert_snapshot!(render(&sample_app()).backend());
    }

    #[test]
    fn suggest_screen() {
        let mut app = sample_app();
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        insta::assert_snapshot!(render(&app).backend());
    }

    #[test]
    fn confirm_screen_shows_rejections_and_warnings() {
        let mut app = sample_app();
        let lib = find(app.tree.as_ref().unwrap(), "Library");
        app.confirm = Some(ConfirmState {
            items: vec![(lib, Verdict::Rejected { path: "/h/Library".into(), reason: "系統或使用者的根目錄本身不可刪除".into() })],
            warnings: vec!["Xcode 正在執行，建議先關閉再清理".into()],
            need_typed: true,
            input: "ye".into(),
        });
        app.screen = Screen::Confirm;
        insta::assert_snapshot!(render(&app).backend());
    }

    #[test]
    fn result_screen() {
        let mut app = sample_app();
        app.results = vec![
            ActionResult {
                path: "/h/Projects/web/node_modules".into(),
                size: 800 << 20,
                handling: Handling::Delete,
                outcome: Outcome::Done,
                output: None,
            },
            ActionResult {
                path: "/h/Downloads/中文 安裝檔.dmg".into(),
                size: 1 << 30,
                handling: Handling::Trash,
                outcome: Outcome::Skipped("掃描後已變動".into()),
                output: None,
            },
        ];
        app.freed = Some(800 << 20);
        app.screen = Screen::Result;
        insta::assert_snapshot!(render(&app).backend());
    }
}
```

- [ ] **Step 2: 確認測試失敗**

`src/lib.rs` 加入 `pub mod ui;`。

Run: `cargo test --lib ui`
Expected: 編譯失敗（`draw`、`pad` 等未定義）

- [ ] **Step 3: 實作 `src/ui/mod.rs`**（加在測試模組上方）

```rust
mod confirm;
mod explore;
mod overlay;
mod result;
mod suggest;

use std::path::Path;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Mode, Screen};
use crate::risk::Risk;
use crate::size::format_size;

pub fn draw(f: &mut Frame, app: &App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(2)]).areas(f.area());
    draw_header(f, header, app);
    if app.tree.is_none() {
        overlay::draw_scanning(f, body, app);
    } else {
        match app.mode {
            Mode::Explore => explore::draw(f, body, app),
            Mode::Suggest => suggest::draw(f, body, app),
        }
    }
    draw_footer(f, footer, app);
    match app.screen {
        Screen::Main => {}
        Screen::Confirm => confirm::draw(f, body, app),
        Screen::Running => overlay::draw_running(f, body),
        Screen::Result => result::draw(f, body, app),
        Screen::Help => overlay::draw_help(f, body),
        Screen::Errors => overlay::draw_errors(f, body, app),
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let root = app.tree.as_ref().map_or_else(|| "…".to_string(), |t| display_path(t.root_path(), &app.ctx.home));
    let disk = app
        .disk
        .map(|d| format!(" · 共 {} · 可用 {}", format_size(d.total), format_size(d.free)))
        .unwrap_or_default();
    let tab = |m: Mode, label: &'static str| {
        if app.mode == m {
            Span::styled(format!("[{label}]"), Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        } else {
            Span::raw(format!(" {label} "))
        }
    };
    let mut spans = vec![
        Span::styled(" disk-sweeper ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!("{root}{disk}  ")),
        tab(Mode::Explore, "Explore"),
        Span::raw(" "),
        tab(Mode::Suggest, "Suggest"),
    ];
    if app.dry_run {
        spans.push(Span::styled("  DRY-RUN", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut info = vec![Span::raw(format!(" 已標記 {} 項 · {}", app.marks.len(), format_size(app.marked_total())))];
    if !app.errors.is_empty() {
        info.push(Span::styled(
            format!("   ⚠ {} 個路徑無法讀取 (u)", app.errors.len()),
            Style::default().fg(Color::Yellow),
        ));
    }
    if app.needs_fda {
        info.push(Span::styled("   需要「完全取用磁碟」權限 (u)", Style::default().fg(Color::Yellow)));
    }
    if let Some(s) = &app.status {
        info.push(Span::styled(format!("   {s}"), Style::default().fg(Color::Cyan)));
    }
    let keys = match app.mode {
        Mode::Explore => " ↑↓/jk 移動  →/l 進入  ←/h 返回  space 標記  tab 切換  d 清理  ? 說明  q 離開",
        Mode::Suggest => " ↑↓/jk 移動  ⏎ 展開  space 標記  a 小項目  tab 切換  d 清理  ? 說明  q 離開",
    };
    let lines = vec![Line::from(info), Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray)))];
    f.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn risk_style(r: Risk) -> Style {
    Style::default().fg(match r {
        Risk::Regenerable => Color::Green,
        Risk::UserData => Color::Yellow,
        Risk::Protected => Color::Red,
        Risk::ToolManaged => Color::Cyan,
    })
}

pub(crate) fn bar(size: u64, total: u64, width: usize) -> String {
    let filled = if total == 0 { 0 } else { ((size as f64 / total as f64) * width as f64).round() as usize };
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// 依終端機顯示寬度補空白或截斷（以 … 結尾），CJK 字元算兩欄。
pub(crate) fn pad(s: &str, width: usize) -> String {
    let w = s.width();
    if w <= width {
        return format!("{s}{}", " ".repeat(width - w));
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > width {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    used += 1;
    out.push_str(&" ".repeat(width.saturating_sub(used)));
    out
}

pub(crate) fn display_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".into(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

pub(crate) fn popup(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let [_, v, _] = Layout::vertical([
        Constraint::Percentage((100 - height_pct) / 2),
        Constraint::Percentage(height_pct),
        Constraint::Min(0),
    ])
    .areas(area);
    let [_, h, _] = Layout::horizontal([
        Constraint::Percentage((100 - width_pct) / 2),
        Constraint::Percentage(width_pct),
        Constraint::Min(0),
    ])
    .areas(v);
    h
}
```

- [ ] **Step 4: 實作 `src/ui/explore.rs`**

```rust
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use super::{bar, display_path, pad, risk_style};
use crate::app::App;
use crate::risk::Risk;
use crate::safety;
use crate::size::format_size;
use crate::tree::NodeKind;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let (Some(tree), Some(&(dir, cursor))) = (&app.tree, app.explore.last()) else { return };
    let dir_path = tree.path_of(dir);
    let total = tree.node(dir).size;
    let items: Vec<ListItem> = app
        .explore_children()
        .into_iter()
        .map(|id| {
            let n = tree.node(id);
            let mark = if app.marks.contains_key(&id) { "✓" } else { " " };
            let arrow = if n.kind == NodeKind::Dir { "▸" } else { " " };
            let mut spans = vec![
                Span::raw(format!("{mark}{arrow} ")),
                Span::raw(pad(&n.name, 36)),
                Span::raw(format!(" {:>7} ", format_size(n.size))),
                Span::raw(bar(n.size, total, 20)),
            ];
            if let Some(s) = app.suggestion_for(id) {
                spans.push(Span::styled(format!("  {} · {}", s.risk.label(), s.rule_id), risk_style(s.risk)));
            } else if safety::protection_reason(&dir_path.join(&*n.name), &app.ctx).is_some() {
                spans.push(Span::styled("  受保護", risk_style(Risk::Protected)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let title = format!(" {} · {} ", display_path(&dir_path, &app.ctx.home), format_size(total));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(cursor));
    f.render_stateful_widget(list, area, &mut state);
}
```

- [ ] **Step 5: 實作 `src/ui/suggest.rs`**

```rust
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use super::{display_path, pad, risk_style};
use crate::app::{App, SuggestRow};
use crate::size::format_size;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(tree) = &app.tree else { return };
    let [list_area, detail_area] = Layout::vertical([Constraint::Min(0), Constraint::Length(4)]).areas(area);
    let rows = app.suggest_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match *row {
            SuggestRow::Header(cat) => {
                let (count, total) =
                    app.visible_items(cat).fold((0usize, 0u64), |(c, t), i| (c + 1, t + app.suggestions[i].size));
                let open = if app.expanded[cat as usize] { "▾" } else { "▸" };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{open} {}", cat.label()), Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(format!("  {count} 項 · {}", format_size(total))),
                ]))
            }
            SuggestRow::Item(i) => {
                let s = &app.suggestions[i];
                let mark = if app.marks.contains_key(&s.node) { "✓" } else { " " };
                let path = display_path(&tree.path_of(s.node), &app.ctx.home);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("   {mark} ")),
                    Span::raw(pad(&path, 56)),
                    Span::raw(format!(" {:>7}  ", format_size(s.size))),
                    Span::styled(s.risk.label(), risk_style(s.risk)),
                ]))
            }
        })
        .collect();
    let hint = if app.show_small { "a：隱藏小於 10MB 的項目" } else { "a：顯示小於 10MB 的項目" };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(" 清理建議 · {hint} ")))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(app.suggest_cursor));
    f.render_stateful_widget(list, list_area, &mut state);

    let detail = match rows.get(app.suggest_cursor) {
        Some(SuggestRow::Item(i)) => {
            let s = &app.suggestions[*i];
            let mut lines = vec![Line::from(format!("{} · {}", s.rule_id, s.description))];
            if let Some(cmd) = s.command {
                lines.push(Line::from(format!("官方指令：{}", cmd.display())));
            }
            lines
        }
        _ => vec![Line::from("選取項目以查看說明；在類別上按 space 可整組標記")],
    };
    f.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: true }).block(Block::default().borders(Borders::ALL)),
        detail_area,
    );
}
```

- [ ] **Step 6: 實作 `src/ui/confirm.rs`**

```rust
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::{display_path, pad, popup};
use crate::app::App;
use crate::risk::Handling;
use crate::safety::Verdict;
use crate::size::format_size;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = &app.confirm else { return };
    let rect = popup(area, 90, 80);
    let max_items = rect.height.saturating_sub(7) as usize;

    let mut item_lines: Vec<Line> = Vec::new();
    let (mut count, mut total) = (0usize, 0u64);
    for (_, v) in &state.items {
        match v {
            Verdict::Approved(t) => {
                count += 1;
                total += t.size();
                let what = match t.handling() {
                    Handling::Command(cmd) => format!("執行 {}", cmd.display()),
                    h => h.label().to_string(),
                };
                item_lines.push(Line::from(vec![
                    Span::styled(format!(" ✓ {} ", pad(&what, 12)), Style::default().fg(Color::Green)),
                    Span::raw(format!("{:>7}  {}", format_size(t.size()), display_path(t.path(), &app.ctx.home))),
                ]));
            }
            Verdict::Rejected { path, reason } => item_lines.push(Line::from(Span::styled(
                format!(" ✗ 略過 {}：{reason}", display_path(path, &app.ctx.home)),
                Style::default().fg(Color::Red),
            ))),
        }
    }
    let hidden = item_lines.len().saturating_sub(max_items);
    item_lines.truncate(max_items);
    let mut lines = item_lines;
    if hidden > 0 {
        lines.push(Line::from(format!(" … 還有 {hidden} 項")));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(" 將處理 {count} 項，共 {}", format_size(total)),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for w in &state.warnings {
        lines.push(Line::from(Span::styled(format!(" ⚠ {w}"), Style::default().fg(Color::Yellow))));
    }
    let prompt = if state.need_typed {
        format!(" 輸入 yes 並按 Enter 執行（Esc 取消）：{}_", state.input)
    } else {
        " 按 y 執行，n 或 Esc 取消".to_string()
    };
    lines.push(Line::from(Span::styled(prompt, Style::default().add_modifier(Modifier::BOLD))));

    let title = if app.dry_run { " 確認清理（DRY-RUN，不會變更檔案） " } else { " 確認清理 " };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(title)),
        rect,
    );
}
```

- [ ] **Step 7: 實作 `src/ui/result.rs`**

```rust
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::{display_path, popup};
use crate::action::Outcome;
use crate::app::App;
use crate::risk::Handling;
use crate::size::format_size;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let rect = popup(area, 90, 80);
    let count = |pred: fn(&Outcome) -> bool| app.results.iter().filter(|r| pred(&r.outcome)).count();
    let done = count(|o| matches!(o, Outcome::Done));
    let dry = count(|o| matches!(o, Outcome::DryRun));
    let skipped = count(|o| matches!(o, Outcome::Skipped(_)));
    let failed = count(|o| matches!(o, Outcome::Failed(_)));

    let dry_note = if dry > 0 { format!(" · 模擬 {dry}") } else { String::new() };
    let mut lines = vec![Line::from(Span::styled(
        format!(" 完成 {done} · 略過 {skipped} · 失敗 {failed}{dry_note}"),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    if let Some(freed) = app.freed {
        let sign = if freed < 0 { "-" } else { "+" };
        lines.push(Line::from(format!(" 可用空間變化：{sign}{}", format_size(freed.unsigned_abs()))));
    }
    if app.results.iter().any(|r| r.handling == Handling::Trash && r.outcome == Outcome::Done) {
        lines.push(Line::from(Span::styled(
            " 移到垃圾桶的項目要清空垃圾桶後才會釋放空間",
            Style::default().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));
    for r in &app.results {
        let (label, color, detail) = match &r.outcome {
            Outcome::Done => ("完成", Color::Green, String::new()),
            Outcome::DryRun => ("模擬", Color::Magenta, String::new()),
            Outcome::Skipped(m) => ("略過", Color::Yellow, format!("：{m}")),
            Outcome::Failed(m) => ("失敗", Color::Red, format!("：{m}")),
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {label} "), Style::default().fg(color)),
            Span::raw(format!("{}{detail}", display_path(&r.path, &app.ctx.home))),
        ]));
        if let Some(out) = &r.output {
            for l in out.lines().take(5) {
                lines.push(Line::from(Span::styled(format!("      {l}"), Style::default().fg(Color::DarkGray))));
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(" 按任意鍵返回"));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 清理結果 ")), rect);
}
```

- [ ] **Step 8: 實作 `src/ui/overlay.rs`**

```rust
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::{bar, display_path, pad, popup};
use crate::app::App;
use crate::size::format_size;

const MAX_ERRORS_SHOWN: usize = 200;

pub fn draw_scanning(f: &mut Frame, area: Rect, app: &App) {
    let p = &app.progress;
    let mut lines = vec![
        Line::from(Span::styled(" 掃描中…", Style::default().add_modifier(Modifier::BOLD))),
        Line::from(format!(" 已掃描 {} 個項目 · {} · 無法讀取 {}", p.files, format_size(p.bytes), p.errors)),
        Line::from(Span::styled(
            format!(" {}", display_path(&p.current, &app.ctx.home)),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];
    let max = p.top.first().map_or(0, |t| t.1);
    for (name, size) in &p.top {
        lines.push(Line::from(format!(" {} {:>7} {}", pad(name, 30), format_size(*size), bar(*size, max, 20))));
    }
    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 掃描 ")), area);
}

pub fn draw_running(f: &mut Frame, area: Rect) {
    let rect = popup(area, 40, 20);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(" 執行中，請稍候…").block(Block::default().borders(Borders::ALL).title(" 清理 ")),
        rect,
    );
}

pub fn draw_help(f: &mut Frame, area: Rect) {
    let lines: Vec<Line> = [
        " Explore：瀏覽目錄大小；space 可標記任何項目（未比對到規則的一律移到垃圾桶）",
        " Suggest：依內建規則列出可清理項目；在類別上按 space 整組標記",
        "",
        " ↑↓ / j k    移動",
        " → / l / ⏎   進入目錄（Suggest：展開或收合類別）",
        " ← / h       返回上層",
        " space       標記 / 取消標記",
        " a           顯示 / 隱藏小於 10MB 的建議",
        " tab         切換 Explore / Suggest",
        " d           檢視並執行清理",
        " u           無法讀取的路徑",
        " q / Ctrl-C  離開",
        "",
        " 可重建＝直接刪除 · 使用者資料＝移到垃圾桶 · 工具管理＝執行官方指令 · 受保護＝不可刪除",
        "",
        " 按任意鍵返回",
    ]
    .into_iter()
    .map(Line::from)
    .collect();
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 說明 ")), area);
}

pub fn draw_errors(f: &mut Frame, area: Rect, app: &App) {
    let mut lines = Vec::new();
    if app.needs_fda {
        lines.push(Line::from(Span::styled(
            " 部分受 macOS 保護的目錄無法讀取。若要完整掃描：系統設定 → 隱私權與安全性 → 完全取用磁碟，加入你的終端機 App 後重新啟動。",
            Style::default().fg(Color::Yellow),
        )));
        lines.push(Line::from(""));
    }
    for e in app.errors.iter().take(MAX_ERRORS_SHOWN) {
        lines.push(Line::from(format!(" {}：{}", display_path(&e.path, &app.ctx.home), e.message)));
    }
    if app.errors.len() > MAX_ERRORS_SHOWN {
        lines.push(Line::from(format!(" … 還有 {} 項", app.errors.len() - MAX_ERRORS_SHOWN)));
    }
    let title = format!(" 無法讀取的路徑（{}） · 按任意鍵返回 ", app.errors.len());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}
```

- [ ] **Step 9: 產生 snapshot 並檢查**

Run: `INSTA_UPDATE=always cargo test --lib ui`
Expected: 8 passed，並在 `src/ui/snapshots/` 產生 5 個 `.snap` 檔。

逐一打開 `.snap` 檔確認：
- `explore_screen`：列出 `Library`、`Movies`、`Downloads`、`Projects`（依大小排序），`Library` 列顯示「受保護」，header 顯示 `共 228G · 可用 10G`
- `suggest_screen`：三個類別標題；`~/Library/Caches/com.spotify.client`（可重建）、`~/Projects/web/node_modules`（可重建）、`~/Movies/raw.mov` 與 `~/Downloads/中文 安裝檔.dmg`（使用者資料），CJK 路徑後的大小欄位有對齊
- `scanning_screen`：顯示 `12345 個項目 · 5.0G · 無法讀取 2` 與兩列 top 目錄
- `confirm_screen_shows_rejections_and_warnings`：紅色 `✗ 略過 ~/Library：…`、`⚠ Xcode 正在執行…`、`輸入 yes 並按 Enter 執行（Esc 取消）：ye_`
- `result_screen`：`完成 1 · 略過 1 · 失敗 0`、`可用空間變化：+800M`

內容不符時修正程式，**不可**直接接受錯誤的 snapshot。

Run: `cargo test --lib ui`
Expected: 8 passed（不帶 `INSTA_UPDATE`）

- [ ] **Step 10: Commit**

```bash
git add src/ui src/lib.rs
git commit -m "feat: add TUI screens with snapshot tests"
```

---

### Task 9: CLI、事件迴圈、端到端驗證

**Files:**
- Modify: `src/main.rs`（整個取代）
- Modify: `docs/superpowers/specs/2026-09-26-disk-sweeper-design.md`（若本計畫的「與 spec 的差異」尚未同步）

**Interfaces:**
- Consumes: `app::{App, Effect}`、`scan::{scan, ScanEvent, ScanOptions}`、`action::{execute, journal::ActionLog, SystemTrash}`、`disk::disk_info`、`ui::draw`、`size::{parse_size, format_size}`、`context::Context`

- [ ] **Step 1: 寫 `src/main.rs`**

```rust
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

use disk_sweeper::action::{self, journal::ActionLog, SystemTrash};
use disk_sweeper::app::{App, Effect};
use disk_sweeper::context::Context;
use disk_sweeper::safety::ApprovedTarget;
use disk_sweeper::scan::{self, ScanEvent, ScanOptions};
use disk_sweeper::tree::NodeId;
use disk_sweeper::{disk, size, ui};

#[derive(Parser)]
#[command(version, about = "Mac 硬碟空間清理器（TUI）")]
struct Cli {
    /// 要掃描的目錄（預設 $HOME，必須在 $HOME 之下）
    path: Option<PathBuf>,
    /// 不實際變更任何檔案
    #[arg(long)]
    dry_run: bool,
    /// 小於此大小的檔案不建立節點，以節省記憶體（例：64K）
    #[arg(long, value_parser = size::parse_size)]
    min_node_size: Option<u64>,
    /// 只掃描並印出摘要（量測效能用），不啟動 TUI
    #[arg(long)]
    scan_only: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ctx = Context::from_env().context("無法取得 $HOME")?;
    let root = match &cli.path {
        Some(p) => p.canonicalize().with_context(|| format!("找不到路徑 {}", p.display()))?,
        None => ctx.home.clone(),
    };
    if !root.starts_with(&ctx.home) {
        bail!("PATH 必須在 $HOME（{}）之下", ctx.home.display());
    }
    let opts = ScanOptions {
        threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
        min_node_size: cli.min_node_size.unwrap_or(0),
    };

    if cli.scan_only {
        let (tx, _rx) = mpsc::channel();
        let r = scan::scan(&root, &opts, &tx);
        let n = r.tree.node(r.tree.root());
        println!(
            "{} 個項目 · {} · 無法讀取 {} · 耗時 {:.2} 秒",
            n.file_count,
            size::format_size(n.size),
            r.errors.len(),
            r.elapsed.as_secs_f64()
        );
        return Ok(());
    }

    let (tx, rx) = mpsc::channel();
    let scan_root = root.clone();
    std::thread::spawn(move || {
        let result = scan::scan(&scan_root, &opts, &tx);
        let _ = tx.send(ScanEvent::Done(result));
    });

    let now = chrono::Local::now().timestamp();
    let mut app = App::new(ctx, cli.dry_run, disk::disk_info(&root).ok(), now);
    // ratatui::init 會安裝 panic hook，panic 時也會恢復終端機
    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app, &rx, &root);
    ratatui::restore();
    res
}

fn run(terminal: &mut DefaultTerminal, app: &mut App, rx: &mpsc::Receiver<ScanEvent>, root: &Path) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| ui::draw(f, app))?;
        while let Ok(ev) = rx.try_recv() {
            app.on_scan_event(ev);
        }
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if let Some(Effect::Execute(targets)) = app.on_key(key) {
            terminal.draw(|f| ui::draw(f, app))?; // 先畫出「執行中」
            execute(app, targets, root);
        }
    }
    Ok(())
}

fn execute(app: &mut App, targets: Vec<(NodeId, ApprovedTarget)>, root: &Path) {
    let mut log = match ActionLog::open(&app.ctx.log_dir()) {
        Ok(l) => Some(l),
        Err(_) if app.dry_run => None,
        Err(e) => {
            app.on_execute_error(format!("無法寫入操作紀錄（{e}），已取消執行"));
            return;
        }
    };
    let before = disk::disk_info(root).ok();
    let (ids, approved): (Vec<NodeId>, Vec<ApprovedTarget>) = targets.into_iter().unzip();
    let results = action::execute(&approved, app.dry_run, log.as_mut(), &SystemTrash);
    let after = disk::disk_info(root).ok();
    let freed = before.zip(after).map(|(b, a)| a.free as i64 - b.free as i64);
    app.on_executed(ids.into_iter().zip(results).collect(), freed);
}
```

- [ ] **Step 2: 編譯、lint、全部測試**

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```
Expected: clippy 無警告；所有測試通過。clippy 的警告要修掉（不可用 `#[allow]` 蓋過，除非是 `clippy::too_many_lines` 這類純風格建議並在 commit 訊息說明）。

- [ ] **Step 3: CLI 錯誤處理冒煙測試**

```bash
cargo build --release
./target/release/disk-sweeper /etc; echo "exit=$?"
./target/release/disk-sweeper --min-node-size 5X; echo "exit=$?"
./target/release/disk-sweeper --scan-only ~/Projects
```
Expected: 第一個印出「PATH 必須在 $HOME（…）之下」且 exit≠0；第二個印出「無效的單位：X」且 exit≠0；第三個印出 `N 個項目 · X · 無法讀取 N · 耗時 N 秒`。

- [ ] **Step 4: 效能驗證（spec 成功標準）**

```bash
./target/release/disk-sweeper --scan-only            # 先跑一次暖快取
time ./target/release/disk-sweeper --scan-only
time du -sk ~ 2>/dev/null | tail -1
```
這兩個指令可能要跑好幾分鐘，用 `run_in_background` 執行。
Expected: `disk-sweeper` 的 real time ≤ `du` 的 1/3。把兩個數字記在最後的回報裡；如果沒達標，照實回報數字，不要調整量測方式讓它「看起來」達標。

- [ ] **Step 5: TUI 端到端驗證（用 tmux，dry-run，只在 temp 目錄）**

```bash
DEMO="$HOME/disk-sweeper-demo"
mkdir -p "$DEMO/web/node_modules/react" && echo '{}' > "$DEMO/web/package.json"
dd if=/dev/zero of="$DEMO/web/node_modules/react/blob" bs=1m count=20 2>/dev/null
tmux new-session -d -s ds -x 120 -y 30 "./target/release/disk-sweeper --dry-run $DEMO"
sleep 2; tmux capture-pane -p -t ds
```
Expected: 看到 Explore 畫面，header 有 `DRY-RUN`，列出 `web`。接著：

```bash
tmux send-keys -t ds Tab; sleep 0.5; tmux capture-pane -p -t ds      # Suggest：node_modules（可重建）
tmux send-keys -t ds j ' ' d; sleep 0.5; tmux capture-pane -p -t ds  # 確認畫面：直接刪除、按 y 執行
tmux send-keys -t ds y; sleep 1; tmux capture-pane -p -t ds          # 結果：模擬 1
tmux send-keys -t ds x q; sleep 0.5
ls "$DEMO/web/node_modules/react/blob"                              # dry-run：檔案仍在
tail -1 ~/Library/Logs/disk-sweeper/actions.jsonl                   # "dry_run":true
```
Expected: 每一步畫面與註解相符；檔案仍存在；紀錄最後一行 `"dry_run":true`。

接著驗證真正刪除（仍只動 demo 目錄）：

```bash
tmux new-session -d -s ds -x 120 -y 30 "./target/release/disk-sweeper $DEMO"
sleep 2; tmux send-keys -t ds Tab j ' ' d y; sleep 1; tmux capture-pane -p -t ds
tmux send-keys -t ds x q
ls "$DEMO/web/node_modules" 2>&1 | head -1                          # No such file or directory
rm -rf "$DEMO"
```
Expected: 結果畫面顯示「完成 1」與可用空間變化；`node_modules` 已被刪除。

- [ ] **Step 6: 同步 spec 並 commit**

確認 spec 已反映本計畫開頭「與 spec 的差異」的五點（若尚未同步就更新 spec 對應段落）。

```bash
git add src/main.rs docs/
git commit -m "feat: wire CLI, event loop and cleanup execution"
```

---

## Self-Review 紀錄

- **Spec 涵蓋**：§1 成功標準 → Task 9 Step 4（效能）、Task 4（型別層級保證）、Task 3（權限錯誤不中斷）、Task 8 confirm 畫面；§5.1–5.4 → Task 4、6、7；§6 規則清單 → Task 5（每條規則都在 catalog，含優先順序測試）；§7 掃描 → Task 3；§8 UI → Task 7、8；§9 CLI → Task 9；§10 錯誤處理 → Task 6（trash 不 fallback、紀錄不可寫拒絕）、Task 3；§11 測試 → 各 task
- **型別一致性**：`NodeId`、`Risk`、`Handling`、`ToolCommand`、`Candidate`、`ApprovedTarget`、`Verdict`、`Suggestion`、`ActionResult`、`Outcome` 在各 task 的 Interfaces 與程式碼中名稱與欄位一致
- **已知取捨**：清理在 UI 執行緒同步執行（期間畫面停在「執行中」），v1 可接受；目錄的 mtime 在 App 寫入快取時會改變，因此執行中的 App 的快取可能被「略過：掃描後已變動」，這是刻意的安全行為
