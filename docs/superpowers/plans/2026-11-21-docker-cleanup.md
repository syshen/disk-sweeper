# Docker 逐項清理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Suggest 模式新增 Docker 類別，列出可安全移除的 image / volume / container / build cache，確認後逐項移除。

**Architecture:** 獨立的 Docker 通道。`docker.rs` 負責查詢、解析與分級；App 另存 Docker 項目與標記；`safety::review_docker` 產生欄位私有的 `ApprovedDocker`，`action::execute_docker` 只接受它。既有以路徑為鍵的流程不變。

**Tech Stack:** Rust 2021、ratatui、serde_json、insta（快照）。不新增相依套件。

**Spec:** `docs/superpowers/specs/2026-11-21-docker-cleanup-design.md`

## Global Constraints

- 移除指令一律不加 `-f`（`docker builder prune -f` 例外，只為略過互動確認）
- 不使用 `volume prune` / `system prune`
- 放進指令列的名稱與 id 必須符合 `[A-Za-z0-9][A-Za-z0-9_.:/@-]*`
- Docker 查詢逾時 5 秒；失敗不影響其他掃描
- 測試不呼叫真正的 `docker`
- 介面文字為台灣正體中文
- 不 commit（等使用者指示）；每個 task 結束跑 `cargo test`，全部完成後跑 `cargo clippy --all-targets -- -D warnings` 與 `cargo fmt --check`

## Review Focus

1. `docker` 存在但 daemon 沒開 → 類別顯示原因、沒有項目、不 panic（Task 1 測試）
2. 輸出超過 pipe 緩衝（數百個 volume）→ 不可卡死（Task 1：stdout 在另一個執行緒讀取）
3. 掃描後 volume 被新的 container 掛上 → 執行時略過（Task 3 測試）
4. 同一個 image id 有多個 tag → 以 `repo:tag` 移除，只拿掉該 tag（Task 1 測試）
5. 大小字串無法解析（`N/A`）→ 視為 0，不 panic（Task 1 測試）

---

### Task 1: `src/docker.rs` — 查詢、解析、分級

**Files:** Create `src/docker.rs`；Modify `src/lib.rs`、`src/risk.rs`

**Produces:**

```rust
// risk.rs
Risk::DockerData            // label "Docker 資料"
Handling::Docker            // label "Docker 移除"

// docker.rs
pub const RULE_ID: &str = "docker-cleanup";
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
pub const REMOVE_TIMEOUT: Duration = Duration::from_secs(300);

pub enum Object {
    BuildCache,
    Image { reference: String },            // repo:tag；沒有 tag 時是 sha256:…
    Container { id: String, name: String },
    AnonVolumes { names: Vec<String> },
    NamedVolume { name: String },
}
pub struct Item { pub object: Object, pub size: u64, pub full_size: u64, pub risk: Risk, pub description: String }
impl Item {
    pub fn label(&self) -> String;          // 顯示用
    pub fn key(&self) -> String;            // "docker:<kind>:<name>"
    pub fn removable(&self) -> bool;        // risk != Protected
}
pub enum Scan { Ready { context: String, items: Vec<Item> }, Unavailable(String) }

pub trait Runner { fn run(&self, program: &Path, args: &[&str], timeout: Duration) -> Result<String, String>; }
pub struct SystemDocker;                    // impl Runner

pub fn parse_size(s: &str) -> u64;
pub fn classify(json: &str) -> Result<Vec<Item>, String>;   // 依 size 由大到小
pub fn scan(program: Option<&Path>, runner: &dyn Runner) -> Scan;
pub fn valid_name(s: &str) -> bool;
/// 每個要執行的指令：(紀錄用 key, docker 參數)
pub fn commands(object: &Object) -> Vec<(String, Vec<String>)>;
/// 用重查的結果收斂要移除的物件；Err 是略過的原因
pub fn recheck(object: &Object, fresh: &[Item]) -> Result<Object, String>;
```

- [ ] 測試（固定樣本 JSON，內含：使用中與未使用的 image、dangling image、執行中與 exited container、匿名／具名／使用中的 volume、shared 與非 shared 的 build cache）：
  - `parse_size`: `"2.94GB"`→2_940_000_000、`"193kB"`→193_000、`"0B"`→0、`"N/A"`→0
  - 匿名未使用 volume 合成一列、size 為總和、risk `DockerData`
  - 具名未使用 volume → `Protected`；使用中的 volume 不出現
  - 未使用 image：size 用 `UniqueSize`、full_size 用 `Size`、reference 為 `alpine:latest`；dangling 用 `sha256:` id
  - exited container → `DockerData`；running 不出現
  - build cache 只算 `InUse=false && Shared=false`，總和為 0 時沒有該列
  - `scan`：program 為 None → `Unavailable("找不到 docker 指令")`；runner 回 Err → `Unavailable`；無法解析 → `Unavailable`
  - `valid_name` 拒絕 `-f`、空字串、含空白或 `;`
  - `recheck`：volume 少了一部分只留下仍符合的；全部不符合 → Err；image 變成使用中 → Err
- [ ] 實作。`SystemDocker::run`：`Command` + `LC_ALL=C`、stdin null；stdout/stderr 各用一個執行緒讀取；`try_wait` 輪詢到逾時則 kill 並回 Err
- [ ] `cargo test docker`

### Task 2: 安全關卡

**Files:** Modify `src/safety/mod.rs`

**Produces:**

```rust
pub struct ApprovedDocker { /* 私有 */ }
impl ApprovedDocker { pub fn item(&self) -> &docker::Item; pub fn program(&self) -> &Path; }
pub enum DockerVerdict { Approved(ApprovedDocker), Rejected { label: String, reason: String } }
pub fn review_docker(item: docker::Item, ctx: &Context) -> DockerVerdict;
```

- [ ] 測試：`Protected` 項目被拒；PATH 沒有 docker 被拒；名稱含可疑字元被拒；正常項目核准且 `program()` 是絕對路徑
- [ ] 實作；`cargo test safety`

### Task 3: 執行

**Files:** Modify `src/action/mod.rs`

**Produces:**

```rust
pub fn execute_docker(targets: &[ApprovedDocker], dry_run: bool, log: Option<&mut ActionLog>, runner: &dyn docker::Runner) -> Vec<ActionResult>;
```

`ActionResult.path` 放 `item.label()`，`handling` 為 `Handling::Docker`；紀錄的 `path` 用指令的 key、`handling` 為 `"docker"`、`rule` 為 `docker::RULE_ID`。

- [ ] 測試（假 runner 記錄所有呼叫）：
  - dry-run：不呼叫 runner，outcome `DryRun`，output 含「將執行：docker volume rm …」
  - 正常：先查一次 `system df`，再逐個 `volume rm <name>`；指令不含 `-f`
  - 重查後 volume 已被使用 → 不移除該個；全部不符合 → `Skipped`
  - 某個 `volume rm` 失敗 → 其餘照做，outcome `Failed`，output 註明成功與失敗數
  - 重查失敗 → `Failed`，不執行任何移除
  - 非 dry-run 且 log 為 None，或 pending 紀錄寫入失敗 → `Failed(LOG_FAIL_MSG)`，不執行
- [ ] 實作；`cargo test action`

### Task 4: App 狀態

**Files:** Modify `src/rules/mod.rs`（`Category::Docker`，排在 Worktree 之後，label `"Docker"`）、`src/scan/mod.rs`（`ScanEvent::Docker(docker::Scan)`）、`src/app.rs`

**Produces:**

```rust
pub docker: Option<docker::Scan>, pub docker_pending: bool, pub docker_marks: BTreeSet<usize>
SuggestRow::Docker(usize)
ConfirmState.docker: Vec<DockerVerdict>
Effect::Execute(Vec<(NodeId, ApprovedTarget)>, Vec<ApprovedDocker>)
pub fn expect_docker(&mut self);                 // 設 docker_pending
pub fn docker_items(&self) -> &[docker::Item];
pub fn marked_count(&self) -> usize;             // marked_total 也要含 Docker
pub fn on_executed(&mut self, done: Vec<(NodeId, ActionResult)>, docker_done: Vec<(String, ActionResult)>, freed: Option<i64>);
```

- [ ] 測試：
  - `Docker` 事件後 Suggest 列出項目；小於 10MB（以 full_size 判斷）預設隱藏
  - `Protected` 項目無法標記並顯示原因
  - 在 Docker 標題按 space 只標 `Regenerable`
  - 只標 Docker 項目也能按 `d`；含 `DockerData` 時必須輸入 `yes`
  - 執行完成（`Done`）的項目從清單消失、標記清空；`begin_rescan` 清掉 Docker 狀態
- [ ] 實作；`cargo test app`

### Task 5: UI

**Files:** Modify `src/ui/suggest.rs`、`confirm.rs`、`overlay.rs`、`mod.rs`

- [ ] Suggest：標題 `Docker（<context>）`，加上「檢查中…」或不可用的原因；項目列與說明區（說明＋將執行的指令）
- [ ] Confirm：Docker 項目列在路徑項目之後，格式相同；被拒絕的顯示原因
- [ ] 說明畫面補一行；`risk_style` 加 `DockerData`；footer 用 `marked_count()`
- [ ] 快照測試：Suggest 含 Docker 項目、Confirm 含 Docker 項目；更新受影響的既有快照（`cargo insta` 不可用時以 `INSTA_UPDATE=always` 產生後人工檢視）

### Task 6: 接線與文件

**Files:** Modify `src/main.rs`、`src/rules/catalog.rs`、`src/action/procs.rs`、`README.md`、舊 spec 中 Docker 規則的描述

- [ ] `spawn_scan`：另開執行緒做 `docker::scan` 並送出 `ScanEvent::Docker`；啟動與重新掃描時呼叫 `expect_docker`
- [ ] `execute`：路徑項目之後執行 Docker 項目，共用同一個 log
- [ ] `--scan-only` 印出 Docker 項目
- [ ] 目錄規則 `docker`：`command: None`，說明指向 Docker 類別；`procs::APPS` 移除對應項目，修正相關測試
- [ ] README：新增「Docker」章節、風險分級與安全模型的對應說明
- [ ] `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`；用 `--scan-only` 與 `--dry-run` 在本機實際驗證
