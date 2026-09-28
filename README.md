# disk-sweeper

A fast, safety-first TUI disk cleaner for macOS, written in Rust.
快速、以安全為優先的 macOS 終端機（TUI）硬碟空間清理工具。

- **看懂空間去哪了**：平行掃描 `$HOME`，逐層瀏覽哪些目錄最佔空間
- **安全地清出空間**：內建規則找出可以清掉的東西（Xcode DerivedData、`node_modules`、套件管理器快取、App 快取、舊安裝檔……），標出風險等級與刪除後果，確認後才動手

介面文字為台灣正體中文。

## 安裝

需要 macOS 與 [Rust](https://rustup.rs) toolchain。

```bash
cargo install --git https://github.com/syshen/disk-sweeper
```

或從原始碼建置：

```bash
git clone https://github.com/syshen/disk-sweeper
cd disk-sweeper
cargo build --release
install -m 755 target/release/disk-sweeper ~/.local/bin/
```

## 使用

```bash
disk-sweeper --dry-run      # 模擬模式：流程完整，但不會變更任何檔案（第一次建議先用這個）
disk-sweeper                # 掃描整個 $HOME
disk-sweeper ~/Projects     # 只掃描某個目錄（必須在 $HOME 之下）
disk-sweeper --scan-only    # 只掃描並印出摘要，不開 TUI
disk-sweeper --min-node-size 64K   # 小檔案不建立節點，節省記憶體
```

掃描完成後會直接進入 **Suggest** 模式列出清理建議；按 `Tab` 可切換到 **Explore** 模式瀏覽目錄大小。

| 按鍵 | 功能 |
|---|---|
| `↑↓` / `j k` | 移動 |
| `→` / `l` / `⏎` | 進入目錄（Suggest：展開或收合類別） |
| `←` / `h` | 返回上層 |
| `space` | 標記／取消標記（在類別標題上按則整組標記） |
| `a` | 顯示／隱藏小於 10MB 的建議 |
| `Tab` | 切換 Explore／Suggest |
| `d` | 檢視並執行清理 |
| `u` | 無法讀取的路徑 |
| `?` | 說明 |
| `q` / `Ctrl-C` | 離開 |

## 安全模型

所有刪除都必須通過單一的安全關卡（`safety::review`），它產生的 `ApprovedTarget` 是執行模組唯一接受的型別——從型別上就無法繞過檢查。

- **只動 `$HOME` 之下、同一個磁碟區的東西**，永遠不需要也不使用 `sudo`
- **硬性禁區**（寫死、不可設定）：`$HOME` 與 `~/Library`、`~/Documents` 等根目錄本身；Keychains、iCloud Drive、Mail、Messages、Preferences、`.ssh`、`.gnupg`、`.config`、`.git` 內部、照片圖庫等整棵子樹。比對前會先把路徑解析成磁碟上的真實名稱，擋掉 APFS 大小寫與 Unicode 變體的繞過
- **風險分級**：
  - 可重建（快取、build 產物）→ 直接刪除
  - 使用者資料 → 移到垃圾桶（可用 Finder「放回原處」救回）
  - 工具管理（Docker、Simulator、Homebrew、pnpm）→ 不刪檔，改執行官方清理指令
  - 受保護 → 無法標記
- **執行前再檢查**：路徑在掃描後被改過或已消失就略過；相關 App 的執行狀態會提出警告
- **確認**：列出每一項的路徑、大小、風險與規則說明；含垃圾桶項目或總量超過 10GB 時需輸入 `yes`
- **操作紀錄**：每一項動手前後都寫入 `~/Library/Logs/disk-sweeper/actions.jsonl`；紀錄寫不進去就拒絕執行
- 移到垃圾桶失敗時只回報錯誤，絕不改為直接刪除；符號連結永遠不會被跟隨

完整設計見 [docs/superpowers/specs/2026-09-26-disk-sweeper-design.md](docs/superpowers/specs/2026-09-26-disk-sweeper-design.md)。

## macOS 權限

- 第一次讀取「桌面」「文件」「下載項目」等資料夾時，macOS 會跳出權限對話框，按下之前掃描會停在那裡（畫面會提示）
- 若想完整掃描受保護的目錄（Mail、Safari 等），到「系統設定 → 隱私權與安全性 → 完全取用磁碟」加入你的終端機 App
- 第一次移到垃圾桶時，macOS 可能會詢問是否允許終端機控制 Finder

## 開發

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

測試一律使用暫存目錄作為 home，不會碰到真正的 `$HOME` 或垃圾桶。

## License

[MIT](LICENSE)
