# disk-sweeper 設計文件

- 日期：2026-09-26
- 狀態：待審閱
- 使用者：本人自用（單一使用者、macOS）

## 1. 目標與成功標準

一個 Mac 上的 TUI 硬碟空間清理器，兩個目標：

1. **了解**：快速看出 `$HOME` 的空間被什麼佔用（Explore 模式）
2. **清理**：根據內建規則建議可安全清理的項目，經確認後安全地釋放空間（Suggest 模式）

**安全** = 不會誤刪重要檔案、不碰關鍵路徑與系統檔案、刪除前給予清楚建議（大小、風險、後果）。
**高效** = 平行、非同步掃描，UI 在掃描中不卡頓。

成功標準：

- 掃描整個 `$HOME` 的時間 ≤ `du -sh ~` 的 1/3（同一台機器、暖快取）
- 任何路徑都無法繞過 safety 模組被刪除（型別層級保證 + 測試覆蓋）
- 掃描時遇到權限錯誤不會卡住或中斷
- 使用者在執行前能看到每一項的路徑、大小、風險等級與處理方式

## 2. 範圍

### v1 包含

- Explore 模式：目錄樹瀏覽、依大小排序、標記
- Suggest 模式，規則類別：
  1. 開發工具（Xcode、Simulator、node_modules、套件管理器 cache、Docker/OrbStack）
  2. 系統與 App cache（`~/Library/Caches`、Homebrew cache、`~/Library/Logs`）
  3. 大型使用者檔案（Downloads 舊檔、`.dmg`/`.pkg`、iOS 備份、超大檔案）
- 風險分級刪除：直接刪除 / 移到垃圾桶 / 禁止
- `--dry-run`、操作紀錄（JSONL）

### v1 不包含（明確排除）

- 需要 sudo 的任何操作（Time Machine 本機快照、`/Library`、`/private/var`）
- 重複檔案偵測
- 掃描結果快取
- `$HOME` 以外的路徑（外接硬碟、其他使用者）
- 自訂規則設定檔

## 3. 技術選型

- 語言：Rust（stable），單一 binary `disk-sweeper`
- 主要 crate：
  - `ratatui` + `crossterm`：TUI
  - `jwalk`：平行目錄走訪（rayon）
  - `clap`：CLI 參數
  - `trash`：移到 macOS 垃圾桶（預設走 Finder，支援「放回原處」；第一次使用時 macOS 會詢問是否允許終端機控制 Finder）
  - `serde` + `serde_json`：操作紀錄
  - `anyhow` / `thiserror`：錯誤處理
  - 測試：`tempfile`、`proptest`、`insta`（TUI snapshot）

## 4. 架構

```
src/
  main.rs          CLI 解析、啟動 app
  app.rs           App 狀態與事件迴圈（UI 執行緒）
  scan/            掃描引擎（背景執行緒）
  tree.rs          arena 目錄樹
  rules/           規則定義與比對
  safety/          安全守門員（唯一關卡）
  action/          執行刪除 / 垃圾桶 / 官方指令、寫紀錄
  ui/              ratatui 畫面元件
```

### 4.1 資料流

1. `main` 解析參數 → 啟動掃描執行緒 → 啟動 UI 事件迴圈
2. 掃描執行緒透過 `std::sync::mpsc` 傳送 `ScanEvent`：
   - `Progress { files, bytes, current_path }`（約每 100ms 一次）
   - `Error { path, kind }`（權限錯誤等）
   - `Done(Tree)`
3. UI 迴圈每約 50ms 處理一次輸入與事件並重畫
4. 掃描完成 → 規則引擎在樹上跑一次，產生 `Vec<Suggestion>`
5. 使用者在兩個模式中標記 → 共用 `CleanupList`
6. 按 `d` → `safety::review(CleanupList) -> Vec<Reviewed>` → 確認畫面 → `action::execute(Vec<ApprovedTarget>)` → 結果 + 從樹上扣除大小

### 4.2 模組介面

- `tree::Tree`：`nodes: Vec<Node>`，`Node { name: Box<str>, parent: Option<NodeId>, children: Vec<NodeId>, size: u64, file_count: u64, kind: NodeKind, inode: u64, dev: u64, mtime: i64 }`。提供 `path_of(NodeId) -> PathBuf`、`children_sorted_by_size(NodeId)`、`subtract(NodeId)`（刪除後向上扣除大小）
- `rules::Rule`：`{ id, category, risk, description, matcher, handling }`；`rules::evaluate(&Tree) -> Vec<Suggestion>`
- `safety::review(Candidate, &Context) -> Verdict`，`Verdict = Approved(ApprovedTarget) | Rejected { path, reason }`
- `safety::ApprovedTarget`：欄位私有、只能在 `safety` 內建立；帶有解析後的 `path`、`handling`、審核時 lstat 的 `dev`、掃描時的 `inode`/`mtime`/`size`，命令類另帶審核時在 PATH 找到的指令絕對路徑（執行時只跑這個路徑）
- `action::execute(&[ApprovedTarget], dry_run: bool, Option<&mut ActionLog>, &dyn Trasher) -> Vec<ActionResult>`

## 5. 安全模型

### 5.1 第 1 層：硬性禁區（寫死，不可設定）

路徑一律先 canonicalize（解析 `..`；父路徑解析 symlink，目標本身不跟隨）後檢查。

- 只允許 `$HOME` 之下的路徑，且不可等於 `$HOME`
- 下列路徑**本身**禁止刪除（其內容物可依規則處理）：
  `~/Library`、`~/Documents`、`~/Desktop`、`~/Downloads`、`~/Pictures`、`~/Music`、`~/Movies`、`~/Applications`、`~/Library/Caches`、`~/Library/Application Support`、`~/Library/Containers`、`~/Library/Group Containers`、`~/Library/Developer`、`~/Library/Logs`
- 下列路徑**及其所有內容**禁止刪除：
  `~/Library/Keychains`、`~/Library/Mobile Documents`（iCloud Drive）、`~/Library/Mail`、`~/Library/Messages`、`~/Library/Accounts`、`~/Library/Cookies`、`~/Library/Preferences`、`~/Library/CloudStorage`、`~/Library/Logs/disk-sweeper`（本工具的紀錄檔）、`~/.ssh`、`~/.gnupg`、`~/.config`、任何 `*.photoslibrary`、任何 `.git` 目錄本身
- 位於 git 工作目錄內且未比對到 🟢 可重建規則的路徑 → 最高只能是 🟡（垃圾桶）
- `review` 會把每個路徑元件解析成磁碟上實際的拼寫（APFS 查找時會做 Unicode 大小寫折疊，例如 U+212A KELVIN SIGN 等同 `K`），再做禁區比對；不存在的路徑一律拒絕
- symlink：只刪除連結本身，永不跟隨；只能經由 Delete（比對到 🟢 規則）移除，不可移到垃圾桶（「符號連結無法移到垃圾桶」）
- 與掃描根目錄不同 `st_dev` 的路徑：掃描時跳過
- 目標的 `st_dev` 與 `$HOME` 不同（掛載點及其內容）→ `review` 拒絕：「位於其他磁碟區，不處理」

### 5.2 第 2 層：風險分級

| 等級 | 意義 | 處理方式 |
|---|---|---|
| 🟢 `Regenerable` | 工具/App 會自行重建 | 直接刪除（`remove_dir_all` / `remove_file`） |
| 🟡 `UserData` | 使用者資料，可能有價值 | 移到垃圾桶 |
| 🔴 `Protected` | 禁區 | 無法標記，UI 顯示原因 |
| ⚙ `ToolManaged` | 由工具管理的資料（Docker、Simulator） | 不做檔案刪除，只提供官方清理指令 |

Explore 模式手動標記的項目：若比對到 🟢 規則則沿用，否則一律 🟡。

### 5.3 第 3 層：執行時檢查

- 標記後、確認前路徑已消失 → 確認畫面顯示為「略過（路徑不存在）」，不會執行
- 確認後、執行前重新 `lstat`：已消失 → Skipped「已不存在」；`dev`、`inode` 或 `mtime` 與審核／掃描時不同 → Skipped「掃描後已變動」
- 相關 app 狀態（以 `pgrep -x` 檢查程序名稱）→ 在確認畫面顯示警告：直接刪檔的規則在 Xcode、Simulator 執行中時建議先關閉；用官方指令清理的 Docker 則相反，Docker Desktop 未執行時警告「docker 指令會失敗」
- 確認畫面列出每一項的風險等級（彩色）、處理方式、大小、路徑，以及規則說明（第二行）；清單可用 ↑↓ / PgUp / PgDn 捲動，放不下時顯示「第 a–b 項，共 N 項」；摘要、所有警告與輸入提示固定在底部，不會被裁掉
- 包含任何 🟡 項目或總量 > 10 GB → 需輸入 `yes` 再按 Enter；否則只有按 `y` 會執行（Enter 不會）
- `--dry-run`：流程完整，但 `action` 不做任何檔案系統變更，結果標示為 dry-run

### 5.4 第 4 層：紀錄

`~/Library/Logs/disk-sweeper/actions.jsonl`，JSONL、只附加：

```json
{"ts":"2026-09-26T10:00:00+08:00","path":"...","size":123,"handling":"delete|trash|command","rule":"xcode-derived-data","result":"ok|skipped|error|pending","detail":"...","dry_run":false}
```

- 真的會動手的目標：動手前先寫一行 `result: "pending"`（意圖），動手後再寫一行結果；`pending` 寫不進去就不動手
- dry-run 與略過的目標：只寫一行結果

## 6. 規則清單（v1）

路徑皆相對於 `$HOME`。

### 6.1 開發工具

| id | 比對 | 風險 |
|---|---|---|
| `xcode-derived-data` | `Library/Developer/Xcode/DerivedData/*` | 🟢 |
| `xcode-archives` | `Library/Developer/Xcode/Archives/*` | 🟡 |
| `xcode-device-support` | `Library/Developer/Xcode/iOS DeviceSupport/*`（及 watchOS/tvOS） | 🟢 |
| `simulator` | `Library/Developer/CoreSimulator` | ⚙ `xcrun simctl delete unavailable` |
| `node-modules` | 名為 `node_modules`、同層有 `package.json`、不在另一個 `node_modules` 內，且路徑中沒有 `.` 開頭的目錄或 `Library`（避免刪到全域安裝的套件與編輯器擴充套件） | 🟢 |
| `rust-target` | 名為 `target`、同層有 `Cargo.toml`，路徑限制同 `node-modules` | 🟢 |
| `npm-cache` | `.npm/_cacache` | 🟢 |
| `bun-cache` | `.bun/install/cache` | 🟢 |
| `pnpm-store` | `Library/pnpm/store` | ⚙ `pnpm store prune` |
| `yarn-cache` | `Library/Caches/Yarn` | 🟢 |
| `cargo-registry-cache` | `.cargo/registry/cache`、`.cargo/registry/src` | 🟢 |
| `gradle-cache` | `.gradle/caches` | 🟢 |
| `pip-cache` | `Library/Caches/pip` | 🟢 |
| `xdg-cache` | `.cache/*` | 🟢 |
| `docker` | `Library/Containers/com.docker.docker` | ⚙ `docker system prune` |
| `orbstack` | `.orbstack` / `Library/Group Containers/*.orbstack` | ⚙ 顯示大小與建議，不提供自動指令 |

⚙ 項目：只有在指令存在於 `$PATH` 時才提供「執行」選項；執行前顯示完整指令並需確認；stdout/stderr 顯示在結果畫面。

### 6.2 系統與 App cache

| id | 比對 | 風險 |
|---|---|---|
| `homebrew-cache` | `Library/Caches/Homebrew` | ⚙ `brew cleanup --prune=all` |
| `app-caches` | `Library/Caches/*`（每個子目錄一項） | 🟢 |
| `user-logs` | `Library/Logs/*`（排除 `disk-sweeper` 自身） | 🟢 |

較具體的規則優先於 `app-caches`（例如 `homebrew-cache`、`yarn-cache`、`pip-cache`）。

### 6.3 大型使用者檔案

| id | 比對 | 風險 |
|---|---|---|
| `installers` | `Downloads/**/*.dmg`、`*.pkg`、`*.iso` | 🟡 |
| `old-downloads` | `Downloads/*` 且 mtime > 90 天、大小 ≥ 50 MB | 🟡 |
| `ios-backups` | `Library/Application Support/MobileSync/Backup/*` | 🟡 |
| `large-files` | 任何 ≥ 1 GB 的一般檔案（且不在禁區、未被其他規則涵蓋） | 🟡 |

### 6.4 規則比對細節

- 單次走訪樹，每個節點依本節表格順序比對規則，第一個符合者勝出（因此 `installers` 優先於 `old-downloads`，具體規則優先於 `app-caches`，`large-files` 最後）；比對到的節點不再往下比對（避免父子重複計算）
- 每條規則附 `description`：說明內容是什麼、刪除後果（例：「Xcode 會在下次 build 時重建，第一次 build 會較慢」）
- Suggest 列表依類別分組，組內依大小排序；小於 10 MB 的項目預設隱藏

## 7. 掃描引擎

- `jwalk::WalkDir`，`parallelism = RayonNewPool(num_cpus)`，`follow_links(false)`，`skip_hidden(false)`
- 以 `process_read_dir` 在走訪時過濾：不同 `st_dev` 的目錄跳過
- 大小 = `st_blocks * 512`；`st_nlink > 1` 的檔案以 `(dev, inode)` 去重，只算第一次
- 走訪完成後由葉往根累加 `size` 與 `file_count`
- 錯誤：`EACCES`/`EPERM` 記錄後繼續；若 `~/Library/Mail`、`~/Library/Safari` 等 TCC 保護路徑讀取失敗，UI 顯示「完全取用磁碟」授權提示
- TCC 同意對話框（例如「允許存取『文件』檔案夾」）出現時，`open()` 會阻塞直到使用者回應；掃描進度的目前路徑超過 3 秒沒變時，UI 顯示「可能正在等待 macOS 權限對話框」提示
- 選配 `--min-node-size <SIZE>`：小於此值的檔案不建立節點，只計入父目錄的大小與檔案數
- 記憶體預估：每節點約 40–60 bytes，200 萬節點約 100–150 MB

## 8. UI

```
┌ disk-sweeper ─ ~/  228G total · 10G free ─────── [Explore] Suggest ┐
│ ▸ Library                    58.2G ████████████████░░░░            │
│   Projects                   31.0G ████████░░░░░░░░░░░░            │
│ ✓ .bun                        3.6G █░░░░░░░░░░░░░░░░░░░  🟢 cache  │
├────────────────────────────────────────────────────────────────────┤
│ Marked: 3 items · 11.2G   ⚠ 214 paths unreadable (u)               │
│ ↑↓ move  →/⏎ enter  ← back  space mark  tab mode  d clean  ? help  │
└────────────────────────────────────────────────────────────────────┘
```

- 頂部：掃描根目錄、磁碟總量/可用（`statfs`）、目前模式
- 掃描中：顯示進度（檔案數、位元組、目前路徑）與目前累計最大的前幾個目錄
- 風險等級以彩色文字標籤顯示（可重建 / 使用者資料 / 工具管理 / 受保護），不用 emoji，避免終端機寬度不一致破壞對齊
- 按鍵：`↑↓/jk` 移動、`→/⏎/l` 進入、`←/h` 返回、`space` 標記、`Tab` 切換模式、`d` 清理、`u` 無法讀取清單、`?` 說明、`q` 離開
- Suggest 模式：可展開的類別群組；在群組上按 `space` 標記整組（🔴 除外）
- 確認畫面 → 執行中進度 → 結果畫面（成功/跳過/失敗數、實際釋放空間 = 執行前後 `statfs` 可用空間差）
- 執行中畫面提示：第一次移到垃圾桶時，macOS 可能會詢問是否允許控制 Finder（Automation 權限）
- 執行期間按下的鍵會被丟棄；結果畫面只有 Enter、Esc、q 會關閉
- 終端機縮放時重新排版；離開時一定恢復終端機狀態（含 panic hook）

## 9. CLI

```
disk-sweeper [PATH]            # 預設 $HOME；PATH 必須在 $HOME 之下
  --dry-run                    # 不實際變更檔案
  --min-node-size <SIZE>       # 例：64K
  --scan-only                  # 只掃描並印出摘要（量測效能用），不啟動 TUI
```

## 10. 錯誤處理

- 掃描錯誤：不中斷，累計顯示
- 刪除錯誤：單項失敗不影響其他項，記入結果與紀錄
- `trash` 失敗：回報錯誤，**不**退而改為直接刪除
- 紀錄檔寫入失敗：在執行前檢查紀錄目錄是否可寫；不可寫則拒絕執行（dry-run 除外）
- 執行途中紀錄檔寫入失敗：本項與之後所有目標一律停止、不動手，回報「無法寫入操作紀錄，拒絕執行」（附上 io 錯誤）

## 11. 測試策略

- `safety`：每條禁區規則的單元測試；`proptest` 隨機產生含 `..`、symlink、unicode、尾端斜線的路徑，驗證禁區路徑永遠得到 `Rejected`
- `rules`：以 `tempfile` 建立假的 `$HOME` 結構，驗證比對結果與優先順序
- `scan`：已知大小的目錄樹驗證加總、硬連結去重、權限錯誤不中斷
- `action`：只在 temp 目錄中操作；驗證 inode/mtime 變動會跳過、dry-run 不變更檔案
- `$HOME` 一律透過參數注入（`Context { home: PathBuf }`），測試不讀取真正的 `$HOME`
- UI：ratatui `TestBackend` + `insta` snapshot，涵蓋 Explore、Suggest、確認畫面
