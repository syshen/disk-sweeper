# Docker 逐項清理 設計文件

- 日期：2026-11-21
- 狀態：已實作

## 1. 為什麼

Docker 佔的空間在 VM 磁碟裡，目錄掃描只看得到一個大檔案。現有的 `docker` 規則只會對目前的 docker context 執行 `docker system prune -f`，有兩個問題：

- 顯示的大小是 Docker Desktop 目錄，指令卻打到目前的 context（例如 OrbStack），對象不一致
- `system prune` 不含 volume，而實際上最佔空間的常是沒有 container 使用的匿名 volume

目標：列出 Docker 裡**可以安全釋放**的東西、說清楚後果，確認後逐項移除。

## 2. 目標與非目標

**目標**

- Suggest 模式多一個「Docker」類別，列出可移除的項目與只供參考的項目
- 只移除確認畫面上列出的那批物件；動手前重查一次狀態
- 沿用既有保證：型別上無法繞過安全關卡、每一項動手前後都寫操作紀錄、`--dry-run` 不變更任何東西

**非目標**

- 不處理執行中的 container、使用中的 image 與 volume（不列出）
- 不刪具名 volume（只顯示）
- 不支援多個 context 同時掃描，也不啟動 Docker
- 不回收 Docker Desktop `Docker.raw` 的稀疏空間

**絕對不可以**：用 `-f` 強制移除 image / container / volume；用 `volume prune`、`system prune` 這類「當下符合條件的全部」指令取代明確的物件清單（build cache 例外，見下）。

## 3. 做法的選擇

現有流程全部以「`$HOME` 之下的檔案節點（`NodeId` / 路徑）」為鍵：建議、標記、`Candidate`、`ApprovedTarget`、結果、紀錄。Docker 物件不是路徑。

| 做法 | 取捨 |
|---|---|
| **A. 獨立的 Docker 通道（採用）** | 新模組 + App 另存 Docker 項目與標記 + 安全關卡多一個入口與核准型別。既有的路徑檢查完全不動 |
| B. 把目標一般化成「路徑或 Docker 物件」 | 模型較一致，但要改寫已驗證的安全關卡與所有相關測試 |
| C. 在目錄樹塞假節點 | 改最少，但弄亂 Explore 的大小，破壞「樹裡都是真實路徑」 |

採用 A：風險最低，安全關卡的既有不變量不受影響。

## 4. 掃描

- 用 PATH 裡的 `docker`（`safety::find_in_path`），對目前的 context 執行 `docker system df -v --format json`，另以 `docker context show` 取得 context 名稱顯示在類別標題
- 在掃描執行緒中、目錄掃描完成後執行，結果以新的 `ScanEvent::Docker` 送回；等待中標題顯示「檢查中…」
- 找不到 `docker`、daemon 沒開、逾時（5 秒）或輸出無法解析：類別顯示原因，沒有任何項目，不影響其他掃描
- 大小是 Docker 的人類可讀字串（`2.94GB`、`193kB`，十進位），解析成位元組；解析不了的項目視為 0

## 5. 項目與分級

| 項目 | 條件 | 列數 | 分級 | 移除指令 |
|---|---|---|---|---|
| 匿名 volume | `Links == 0`，且有 `com.docker.volume.anonymous` label 或名稱是 64 位十六進位 | 合併成一列 | 無法復原 | `docker volume rm <name>`（逐個） |
| 沒被使用的 image | `Containers == 0`（含 dangling） | 每個一列 | 可重建 | `docker image rm <repo:tag 或 id>` |
| 已停止的 container | state 為 `exited` / `created` / `dead` | 每個一列 | 無法復原 | `docker container rm <id>` |
| Build cache | `InUse == false` 且 `Shared == false` 的總和 > 0 | 合併成一列 | 可重建 | `docker builder prune -f` |
| 具名 volume | `Links == 0`，不符合匿名條件 | 每個一列 | 受保護（不可標記） | — |

- **大小**：image 用 `UniqueSize`（和 Docker 自己算的 reclaimable 一致），說明中另外標出與其他 image 共用的大小。小項目的隱藏門檻（10MB）對 image 改用完整 `Size` 判斷，避免「獨占很小但共用很大」的 image 被藏起來
- **Build cache** 是唯一用 prune 的：它沒有「逐筆移除」的對應指令，而且內容純粹可重建
- **風險等級**新增 `Risk::DockerData`（畫面上顯示「無法復原」：刪了救不回，無法進垃圾桶）；可重建的 Docker 項目用既有的 `Risk::Regenerable`
- 清單依可釋放的大小排序；類別標題的總量只算可移除的項目
- 類別標記（在標題上按 space）只會標「可重建」的項目；無法復原的必須逐列標記

## 6. 安全關卡與執行

- `safety::review_docker(item, ctx) -> DockerVerdict`，核准後得到欄位私有的 `ApprovedDocker`；`action::execute_docker` 只接受它
- 審核內容：項目必須是可移除的分級；`docker` 必須在 PATH 找到（記下絕對路徑，執行時不再查 PATH）；所有要放進指令列的名稱與 id 必須符合 `[A-Za-z0-9][A-Za-z0-9_.:/@-]*`，否則拒絕（防止被當成選項）
- 確認畫面：只要含無法復原的項目就必須輸入 `yes`
- 執行前**重查一次** `docker system df -v`，用同一套分級重新判斷：已不存在 → 略過；變成使用中 → 略過。合併的匿名 volume 逐個判斷，只移除仍符合條件的
- 透過 `docker::Runner` trait 執行指令（測試注入假的）；指令一律不加 `-f`（`builder prune -f` 的 `-f` 只是略過互動確認）
- `--dry-run`：不重查、不執行，結果列出「將執行：…」
- 操作紀錄：沿用 `ActionRecord`，`path` 欄位寫 `docker:<kind>:<name>`，`handling` 為 `docker`。每個實際被移除的物件一筆（170 個 volume 就是 170 筆），動手前先寫 `pending`；紀錄寫不進去就停手
- 結果畫面每個建議列一筆；合併列的輸出寫「已移除 N 個，略過 M 個」

## 7. 其他變更

- 目錄規則 `docker` 拿掉自動執行的 `docker system prune -f`，變成和 OrbStack 規則一樣只有說明，並指向 Docker 類別；「Docker Desktop 未執行」的確認畫面警告也一併移除
- `--scan-only` 也印出 Docker 項目與判斷
- 說明畫面與 README 補上 Docker 類別

## 8. 測試

- 解析與分級：用真實 `docker system df -v --format json` 輸出做成的固定樣本
- 安全關卡：受保護項目、找不到 docker、含可疑字元的名稱都被拒絕
- 執行：假的 runner 驗證指令內容、重查後略過、紀錄寫入失敗就停手、dry-run 不執行
- App：標記、類別標記只選可重建、需要輸入 yes、執行後項目消失
- UI：Suggest 與確認畫面的快照

測試不會呼叫真正的 `docker`。
