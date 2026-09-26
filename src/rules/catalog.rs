use super::Category::{AppCache, DevTools, LargeFiles};
use super::Matcher::{
    ChildOf, ChildWithSuffix, DirWithSibling, Exact, ExtUnder, LargeFile, OldChild,
};
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
        matcher: ChildOf {
            parents: &["Library/Developer/Xcode/DerivedData"],
            exclude: NONE,
        },
        command: None,
        description: "Xcode 的 build 中間產物，下次 build 會自動重建（第一次 build 會較慢）",
    },
    Rule {
        id: "xcode-archives",
        category: DevTools,
        risk: UserData,
        matcher: ChildOf {
            parents: &["Library/Developer/Xcode/Archives"],
            exclude: NONE,
        },
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
        command: Some(ToolCommand {
            program: "xcrun",
            args: &["simctl", "delete", "unavailable"],
        }),
        description: "iOS Simulator 的裝置與資料；官方指令只會刪除目前 Xcode 已不支援的 simulator",
    },
    Rule {
        id: "node-modules",
        category: DevTools,
        risk: Regenerable,
        matcher: DirWithSibling {
            name: "node_modules",
            sibling: "package.json",
        },
        command: None,
        description: "專案的 npm 套件，執行 npm / pnpm / bun install 即可重建",
    },
    Rule {
        id: "rust-target",
        category: DevTools,
        risk: Regenerable,
        matcher: DirWithSibling {
            name: "target",
            sibling: "Cargo.toml",
        },
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
        command: Some(ToolCommand {
            program: "pnpm",
            args: &["store", "prune"],
        }),
        description:
            "pnpm 共用套件庫；直接刪除會破壞既有專案的硬連結，請用官方指令清理未使用的套件",
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
        matcher: ChildOf {
            parents: &[".cache"],
            exclude: NONE,
        },
        command: None,
        description: "命令列工具的快取目錄（~/.cache），工具會自動重建",
    },
    Rule {
        id: "docker",
        category: DevTools,
        risk: ToolManaged,
        matcher: Exact(&["Library/Containers/com.docker.docker"]),
        command: Some(ToolCommand {
            program: "docker",
            args: &["system", "prune", "-f"],
        }),
        description:
            "Docker Desktop 的 VM 磁碟映像；直接刪檔會失去所有 image 與 volume，請用官方指令清理",
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
        matcher: ChildWithSuffix {
            parent: "Library/Group Containers",
            suffix: ".dev.orbstack",
        },
        command: None,
        description: "OrbStack 的資料；請在 OrbStack 內清理 image 與 container",
    },
    // ── 系統與 App cache ──
    Rule {
        id: "homebrew-cache",
        category: AppCache,
        risk: ToolManaged,
        matcher: Exact(&["Library/Caches/Homebrew"]),
        command: Some(ToolCommand {
            program: "brew",
            args: &["cleanup", "--prune=all"],
        }),
        description: "Homebrew 下載的安裝檔快取",
    },
    Rule {
        id: "app-caches",
        category: AppCache,
        risk: Regenerable,
        matcher: ChildOf {
            parents: &["Library/Caches"],
            exclude: NONE,
        },
        command: None,
        description: "App 的快取，App 會自動重建；若 App 正在執行，少數情況需要重開",
    },
    Rule {
        id: "user-logs",
        category: AppCache,
        risk: Regenerable,
        matcher: ChildOf {
            parents: &["Library/Logs"],
            exclude: &["disk-sweeper"],
        },
        command: None,
        description: "App 與系統的 log 檔",
    },
    // ── 大型使用者檔案 ──
    Rule {
        id: "installers",
        category: LargeFiles,
        risk: UserData,
        matcher: ExtUnder {
            under: "Downloads",
            exts: &["dmg", "pkg", "iso"],
        },
        command: None,
        description: "下載的安裝檔，安裝完成後通常不需要保留",
    },
    Rule {
        id: "old-downloads",
        category: LargeFiles,
        risk: UserData,
        matcher: OldChild {
            parent: "Downloads",
            min_age_days: 90,
            min_size: 50 * MB,
        },
        command: None,
        description: "下載資料夾中超過 90 天未修改的大型項目",
    },
    Rule {
        id: "ios-backups",
        category: LargeFiles,
        risk: UserData,
        matcher: ChildOf {
            parents: &["Library/Application Support/MobileSync/Backup"],
            exclude: NONE,
        },
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
