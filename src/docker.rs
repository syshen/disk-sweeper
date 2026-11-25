//! Docker 檢查：列出 Docker 裡可以安全釋放的東西。
//!
//! Docker 的資料在 VM 磁碟裡，目錄掃描看不到內容，所以改問 docker 自己：
//! `docker system df -v` 會列出每個 image / container / volume / build cache
//! 的大小與使用狀態。
//!
//! 會列出的項目：
//! - 沒有 container 使用的**匿名** volume（合成一列）——資料，刪了救不回
//! - 沒有 container 使用的 image（含 dangling）——可重新 pull / build
//! - 已停止的 container——容器內沒存到 volume 的變更會消失
//! - 沒在使用、也沒和 image 共用的 build cache——可重建
//! - 沒有 container 使用的**具名** volume（每個一列）——常是資料庫等資料，刪了救不回
//!
//! 執行中的 container、使用中的 image 與 volume 一律不列出。
//!
//! 移除時只動確認畫面上那一批物件，動手前重查一次（`recheck`），指令一律不加
//! `-f`：就算判斷有誤，docker 自己也會拒絕移除使用中的東西。

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::risk::Risk;
use crate::size::format_size;

pub const RULE_ID: &str = "docker-cleanup";
/// 查詢狀態的逾時：daemon 卡住時不可拖住整個掃描
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// 單一移除指令的逾時
pub const REMOVE_TIMEOUT: Duration = Duration::from_secs(300);

/// 用 template 而不是 `--format json`：舊版 docker CLI 也支援
pub const DF_ARGS: [&str; 5] = ["system", "df", "-v", "--format", "{{json .}}"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Object {
    BuildCache,
    /// `repo:tag`；沒有 tag 時是 `sha256:…`
    Image {
        reference: String,
    },
    Container {
        id: String,
        name: String,
    },
    /// 所有沒被使用的匿名 volume
    AnonVolumes {
        names: Vec<String>,
    },
    NamedVolume {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub object: Object,
    /// 移除後預期釋放的大小（image 為獨占的部分）
    pub size: u64,
    /// 完整大小；決定要不要當成小項目隱藏
    pub full_size: u64,
    pub risk: Risk,
    pub description: String,
}

impl Item {
    pub fn label(&self) -> String {
        match &self.object {
            Object::BuildCache => "Build cache".into(),
            Object::Image { reference } => match reference.strip_prefix("sha256:") {
                Some(hex) => format!("image <none> {}", hex.get(..12).unwrap_or(hex)),
                None => format!("image {reference}"),
            },
            Object::Container { name, .. } => format!("container {name}"),
            Object::AnonVolumes { names } => format!("匿名 volume × {}", names.len()),
            Object::NamedVolume { name } => format!("volume {name}"),
        }
    }

    /// 結果與比對用的識別字串
    pub fn key(&self) -> String {
        match &self.object {
            Object::BuildCache => "docker:build-cache".into(),
            Object::Image { reference } => format!("docker:image:{reference}"),
            Object::Container { id, .. } => format!("docker:container:{id}"),
            Object::AnonVolumes { .. } => "docker:volumes".into(),
            Object::NamedVolume { name } => format!("docker:volume:{name}"),
        }
    }

    pub fn removable(&self) -> bool {
        self.risk != Risk::Protected
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    Ready {
        /// 目前的 docker context（例如 `orbstack`、`desktop-linux`）
        context: String,
        items: Vec<Item>,
    },
    /// 沒有 docker、daemon 沒開或查詢失敗；附原因
    Unavailable(String),
}

/// 執行 docker 指令；測試時注入假的。
pub trait Runner {
    /// 成功回傳 stdout；結束狀態非 0、逾時或無法啟動都回傳 Err
    fn run(&self, program: &Path, args: &[&str], timeout: Duration) -> Result<String, String>;
}

pub struct SystemDocker;

impl Runner for SystemDocker {
    fn run(&self, program: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
        let mut child = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        // 輸出可能超過 pipe 緩衝（幾百個 volume 的 JSON）：一定要邊跑邊讀，否則會互相卡死
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if start.elapsed() > timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("逾時（{} 秒）", timeout.as_secs()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e.to_string()),
            }
        };
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        if status.success() {
            Ok(stdout)
        } else {
            let msg = stderr.trim();
            Err(if msg.is_empty() {
                format!("指令結束狀態：{status}")
            } else {
                msg.to_string()
            })
        }
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// 問 docker 目前有哪些可以清的東西。任何失敗都只回報原因，不影響其他掃描。
pub fn scan(program: Option<&Path>, runner: &dyn Runner) -> Scan {
    let Some(program) = program else {
        return Scan::Unavailable("找不到 docker 指令".into());
    };
    let Ok(json) = runner.run(program, &DF_ARGS, QUERY_TIMEOUT) else {
        return Scan::Unavailable("Docker 未啟動或無法連線".into());
    };
    let Ok(items) = classify(&json) else {
        return Scan::Unavailable("無法解析 docker 的輸出".into());
    };
    let context = runner
        .run(program, &["context", "show"], QUERY_TIMEOUT)
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".into());
    Scan::Ready { context, items }
}

/// docker 的人類可讀大小（`2.94GB`、`193kB`，十進位）轉成位元組；看不懂的當成 0。
pub fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let Ok(n) = s[..split].parse::<f64>() else {
        return 0;
    };
    let unit: String = s[split..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let mult: f64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return 0,
    };
    (n * mult).round() as u64
}

/// 可以直接放進 docker 指令列的名稱或 id：不可以 `-` 開頭（會被當成選項），
/// 只能有 image reference / volume 名稱會用到的字元。
pub fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.:/@-".contains(c))
}

fn field(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn list<'a>(root: &'a Value, key: &str) -> &'a [Value] {
    root.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// volume 的 label 是 `k=v,k=v`；匿名 volume 有 `com.docker.volume.anonymous`（Docker 23 起），
/// 更舊的匿名 volume 沒有 label，但名稱一定是 64 位十六進位。
fn is_anonymous(name: &str, labels: &str) -> bool {
    is_hex64(name)
        || labels
            .split(',')
            .any(|kv| kv.split('=').next() == Some("com.docker.volume.anonymous"))
}

/// 解析 `docker system df -v` 的 JSON，挑出沒被使用的東西並分級；依可釋放的大小由大到小。
pub fn classify(json: &str) -> Result<Vec<Item>, String> {
    let root: Value = serde_json::from_str(json.trim()).map_err(|e| e.to_string())?;
    if !root.is_object() {
        return Err("不是預期的格式".into());
    }
    let mut items = Vec::new();

    let (mut anon, mut anon_size) = (Vec::new(), 0u64);
    for v in list(&root, "Volumes") {
        if field(v, "Links") != "0" {
            continue;
        }
        let name = field(v, "Name");
        let size = parse_size(&field(v, "Size"));
        if is_anonymous(&name, &field(v, "Labels")) {
            anon.push(name);
            anon_size += size;
        } else {
            let labels = field(v, "Labels");
            let project = labels
                .split(',')
                .find_map(|kv| kv.strip_prefix("com.docker.compose.project="))
                .filter(|p| !p.is_empty());
            let owner = match project {
                Some(p) => format!("compose 專案 {p} 的具名 volume"),
                None => "具名 volume".into(),
            };
            items.push(Item {
                description: format!(
                    "{owner}，目前沒有 container 使用 · 可能是資料庫等資料，刪除後無法復原；確定不再需要才移除"
                ),
                object: Object::NamedVolume { name },
                size,
                full_size: size,
                risk: Risk::DockerData,
            });
        }
    }
    if !anon.is_empty() {
        anon.sort();
        items.push(Item {
            object: Object::AnonVolumes { names: anon },
            size: anon_size,
            full_size: anon_size,
            risk: Risk::DockerData,
            description: "沒有任何 container 使用的匿名 volume（多半是 compose down 沒加 -v 或 docker run 留下的）· 逐個執行 docker volume rm，刪除後無法復原".into(),
        });
    }

    for v in list(&root, "Images") {
        if field(v, "Containers") != "0" {
            continue;
        }
        let (repo, tag) = (field(v, "Repository"), field(v, "Tag"));
        let tagged = ![&repo, &tag]
            .iter()
            .any(|s| s.is_empty() || s.as_str() == "<none>");
        let reference = if tagged {
            format!("{repo}:{tag}")
        } else {
            field(v, "ID")
        };
        let since = field(v, "CreatedSince");
        let mut description = if tagged {
            format!("沒有 container 使用的 image（建立於 {since}）· 可重新 pull 或 build")
        } else {
            format!("沒有 tag 也沒有 container 使用的 image（建立於 {since}）· 可重新 build")
        };
        let shared = parse_size(&field(v, "SharedSize"));
        if shared > 0 {
            description.push_str(&format!(
                " · 另有約 {} 與其他 image 共用，共用的 image 都移除後才會釋放",
                format_size(shared)
            ));
        }
        items.push(Item {
            object: Object::Image { reference },
            size: parse_size(&field(v, "UniqueSize")),
            full_size: parse_size(&field(v, "Size")),
            risk: Risk::Regenerable,
            description,
        });
    }

    for v in list(&root, "Containers") {
        if !matches!(field(v, "State").as_str(), "exited" | "created" | "dead") {
            continue;
        }
        let size = parse_size(&field(v, "Size"));
        items.push(Item {
            object: Object::Container {
                id: field(v, "ID"),
                name: field(v, "Names"),
            },
            size,
            full_size: size,
            risk: Risk::DockerData,
            description: format!(
                "已停止的 container（{}，image {}）· 容器內沒存到 volume 的變更會消失；volume 與 image 保留",
                field(v, "Status"),
                field(v, "Image")
            ),
        });
    }

    // 和 image 共用的 cache 清了也不會釋放空間，不算
    let cache: u64 = list(&root, "BuildCache")
        .iter()
        .filter(|v| field(v, "InUse") == "false" && field(v, "Shared") == "false")
        .map(|v| parse_size(&field(v, "Size")))
        .sum();
    if cache > 0 {
        items.push(Item {
            object: Object::BuildCache,
            size: cache,
            full_size: cache,
            risk: Risk::Regenerable,
            description: "沒有在使用的 build cache · 下次 build 會重新產生（較慢）".into(),
        });
    }

    items.sort_by_key(|i| std::cmp::Reverse((i.size, i.full_size)));
    Ok(items)
}

/// 移除這個物件要執行的指令：(操作紀錄用的識別字串, docker 參數)。
/// 一律不加 `-f`；`builder prune -f` 的 `-f` 只是略過互動確認。
pub fn commands(object: &Object) -> Vec<(String, Vec<String>)> {
    let cmd = |key: String, args: &[&str]| (key, args.iter().map(|s| s.to_string()).collect());
    match object {
        Object::BuildCache => vec![cmd(
            "docker:build-cache".into(),
            &["builder", "prune", "-f"],
        )],
        Object::Image { reference } => vec![cmd(
            format!("docker:image:{reference}"),
            &["image", "rm", reference],
        )],
        Object::Container { id, name } => vec![cmd(
            format!("docker:container:{name}"),
            &["container", "rm", id],
        )],
        Object::AnonVolumes { names } => names
            .iter()
            .map(|n| cmd(format!("docker:volume:{n}"), &["volume", "rm", n]))
            .collect(),
        Object::NamedVolume { name } => vec![cmd(
            format!("docker:volume:{name}"),
            &["volume", "rm", name],
        )],
    }
}

/// 所有會放進指令列的名稱與 id
pub fn names(object: &Object) -> Vec<&str> {
    match object {
        Object::BuildCache => Vec::new(),
        Object::Image { reference } => vec![reference],
        Object::Container { id, .. } => vec![id],
        Object::AnonVolumes { names } => names.iter().map(String::as_str).collect(),
        Object::NamedVolume { name } => vec![name],
    }
}

/// 動手前用重查的結果（`classify` 的輸出）收斂要移除的物件：
/// 已不存在或變成使用中的就不動。Err 是略過的原因。
pub fn recheck(object: &Object, fresh: &[Item]) -> Result<Object, String> {
    const GONE: &str = "已不存在或正在使用中";
    match object {
        Object::BuildCache => fresh
            .iter()
            .any(|i| i.object == Object::BuildCache)
            .then(|| object.clone())
            .ok_or_else(|| "已沒有可清的 build cache".into()),
        Object::Image { .. } | Object::NamedVolume { .. } => fresh
            .iter()
            .any(|i| &i.object == object)
            .then(|| object.clone())
            .ok_or_else(|| GONE.into()),
        Object::Container { id, .. } => fresh
            .iter()
            .any(|i| matches!(&i.object, Object::Container { id: f, .. } if f == id))
            .then(|| object.clone())
            .ok_or_else(|| GONE.into()),
        Object::AnonVolumes { names } => {
            let still: Vec<String> = fresh
                .iter()
                .find_map(|i| match &i.object {
                    Object::AnonVolumes { names: f } => Some(f),
                    _ => None,
                })
                .map(|f| names.iter().filter(|n| f.contains(n)).cloned().collect())
                .unwrap_or_default();
            if still.is_empty() {
                Err(GONE.into())
            } else {
                Ok(Object::AnonVolumes { names: still })
            }
        }
    }
}

#[cfg(test)]
pub mod testutil {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    pub const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    pub const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    pub const HEX_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// 取自真實的 `docker system df -v --format '{{json .}}'`（Docker 29 / OrbStack），刪減過
    pub fn sample_json() -> String {
        format!(
            r#"{{
"Images":[
 {{"Containers":"1","CreatedSince":"24 hours ago","ID":"sha256:0bd79cf07f59","Repository":"odoo","Tag":"18","SharedSize":"0B","Size":"2.94GB","UniqueSize":"2.942GB"}},
 {{"Containers":"0","CreatedSince":"2 weeks ago","ID":"sha256:f59abe656b69","Repository":"alpine","Tag":"latest","SharedSize":"0B","Size":"14.6MB","UniqueSize":"14.56MB"}},
 {{"Containers":"0","CreatedSince":"3 weeks ago","ID":"sha256:7d0892fcd348a5e05ab9c26fdbc5259af8db38b60018b8dc75b832796aa741cf","Repository":"<none>","Tag":"<none>","SharedSize":"2.892GB","Size":"2.89GB","UniqueSize":"21.4kB"}}
],
"Containers":[
 {{"ID":"610ea01770aa","Image":"odoo:18","Names":"odoo-addon-18-odoo-1","Platform":null,"Size":"233kB","State":"running","Status":"Up 7 minutes"}},
 {{"ID":"04f5de0623e9","Image":"odoo:19","Names":"odoo-demo-odoo19-1","Platform":null,"Size":"197kB","State":"exited","Status":"Exited (0) 3 weeks ago"}}
],
"Volumes":[
 {{"Labels":"com.docker.volume.anonymous=","Links":"0","Name":"{HEX_A}","Size":"71.93MB"}},
 {{"Labels":"","Links":"0","Name":"{HEX_B}","Size":"48.7MB"}},
 {{"Labels":"com.docker.volume.anonymous=","Links":"1","Name":"{HEX_C}","Size":"191B"}},
 {{"Labels":"com.docker.compose.project=odoo-demo,com.docker.compose.volume=odoo19-db","Links":"0","Name":"odoo-demo_odoo19-db","Size":"143.5MB"}},
 {{"Labels":"com.docker.compose.project=mars","Links":"1","Name":"mars_pg","Size":"1.2GB"}}
],
"BuildCache":[
 {{"ID":"nkdau1","InUse":"false","Shared":"true","Size":"28.9MB"}},
 {{"ID":"zzz","InUse":"false","Shared":"false","Size":"300MB"}},
 {{"ID":"yyy","InUse":"true","Shared":"false","Size":"1GB"}}
]}}"#
        )
    }

    /// 記錄所有呼叫；`system df` 依序回傳 `df` 裡的結果（用完後重複最後一個），
    /// 參數含 `fail` 裡任一字串的指令回傳 Err。
    #[derive(Default)]
    pub struct FakeDocker {
        pub calls: RefCell<Vec<Vec<String>>>,
        pub df: RefCell<VecDeque<Result<String, String>>>,
        pub fail: Vec<String>,
    }

    impl FakeDocker {
        pub fn with_df(df: Vec<Result<String, String>>) -> Self {
            Self {
                df: RefCell::new(df.into()),
                ..Self::default()
            }
        }

        /// 查詢以外（真的會變更東西）的呼叫
        pub fn removals(&self) -> Vec<Vec<String>> {
            self.calls
                .borrow()
                .iter()
                .filter(|c| c[0] != "system" && c[0] != "context")
                .cloned()
                .collect()
        }
    }

    impl Runner for FakeDocker {
        fn run(&self, _: &Path, args: &[&str], _: Duration) -> Result<String, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            if args[0] == "system" {
                let mut df = self.df.borrow_mut();
                return if df.len() > 1 {
                    df.pop_front().unwrap()
                } else {
                    df.front().cloned().unwrap_or(Err("no daemon".into()))
                };
            }
            if args[0] == "context" {
                return Ok("orbstack\n".into());
            }
            if args.iter().any(|a| self.fail.iter().any(|f| f == a)) {
                return Err("volume is in use".into());
            }
            Ok(String::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    fn items() -> Vec<Item> {
        classify(&sample_json()).unwrap()
    }

    fn find<'a>(items: &'a [Item], key: &str) -> &'a Item {
        items
            .iter()
            .find(|i| i.key() == key)
            .unwrap_or_else(|| panic!("no item {key}"))
    }

    #[test]
    fn parse_size_reads_docker_units() {
        assert_eq!(parse_size("2.94GB"), 2_940_000_000);
        assert_eq!(parse_size("193kB"), 193_000);
        assert_eq!(parse_size("14.56MB"), 14_560_000);
        assert_eq!(parse_size("191B"), 191);
        assert_eq!(parse_size("0B"), 0);
        assert_eq!(parse_size("233kB (virtual 2.9GB)"), 233_000);
        assert_eq!(parse_size("1.5GiB"), 1_610_612_736);
        assert_eq!(parse_size("N/A"), 0);
        assert_eq!(parse_size(""), 0);
    }

    #[test]
    fn unused_anonymous_volumes_are_merged_into_one_item() {
        let items = items();
        let v = find(&items, "docker:volumes");
        // 有 label 的、和沒有 label 但名稱是 64 位十六進位的都算匿名；使用中的不算
        assert_eq!(
            v.object,
            Object::AnonVolumes {
                names: vec![HEX_A.to_string(), HEX_B.to_string()]
            }
        );
        assert_eq!(v.size, 71_930_000 + 48_700_000);
        assert_eq!(v.risk, Risk::DockerData);
        assert_eq!(v.label(), "匿名 volume × 2");
    }

    #[test]
    fn unused_named_volume_is_removable_one_by_one() {
        let items = items();
        let v = find(&items, "docker:volume:odoo-demo_odoo19-db");
        assert_eq!(v.risk, Risk::DockerData);
        assert!(v.removable());
        assert!(v.description.contains("compose 專案 odoo-demo"));
        assert_eq!(
            commands(&v.object),
            [(
                "docker:volume:odoo-demo_odoo19-db".to_string(),
                vec!["volume".into(), "rm".into(), "odoo-demo_odoo19-db".into()]
            )]
        );
        // 使用中的具名 volume 不列出
        assert!(items.iter().all(|i| i.key() != "docker:volume:mars_pg"));
    }

    #[test]
    fn unused_images_count_unique_size_and_keep_full_size() {
        let items = items();
        let alpine = find(&items, "docker:image:alpine:latest");
        assert_eq!(
            (alpine.size, alpine.full_size, alpine.risk),
            (14_560_000, 14_600_000, Risk::Regenerable)
        );
        let dangling = items
            .iter()
            .find(|i| i.label() == "image <none> 7d0892fcd348")
            .unwrap();
        assert_eq!((dangling.size, dangling.full_size), (21_400, 2_890_000_000));
        assert!(dangling.description.contains("共用"));
        // 使用中的 image 不列出
        assert!(items.iter().all(|i| i.key() != "docker:image:odoo:18"));
    }

    #[test]
    fn stopped_containers_are_listed_running_ones_are_not() {
        let items = items();
        let c = find(&items, "docker:container:04f5de0623e9");
        assert_eq!(c.risk, Risk::DockerData);
        assert_eq!(c.label(), "container odoo-demo-odoo19-1");
        assert_eq!(
            commands(&c.object)[0].1,
            ["container", "rm", "04f5de0623e9"]
        );
        assert_eq!(
            items
                .iter()
                .filter(|i| matches!(i.object, Object::Container { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn build_cache_counts_only_unused_unshared_entries() {
        let items = items();
        assert_eq!(find(&items, "docker:build-cache").size, 300_000_000);

        let none = r#"{"BuildCache":[{"InUse":"false","Shared":"true","Size":"28.9MB"}]}"#;
        assert!(classify(none).unwrap().is_empty());
    }

    #[test]
    fn items_are_sorted_by_size_desc() {
        let sizes: Vec<u64> = items().iter().map(|i| i.size).collect();
        let mut sorted = sizes.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(sizes, sorted);
    }

    #[test]
    fn classify_tolerates_missing_and_null_sections() {
        assert!(classify(r#"{"Images":null,"Volumes":[]}"#)
            .unwrap()
            .is_empty());
        assert!(classify("not json").is_err());
        assert!(classify("[]").is_err());
    }

    #[test]
    fn removal_commands_never_force() {
        for item in items() {
            for (_, args) in commands(&item.object) {
                let forced = args.iter().any(|a| a == "-f" || a == "--force");
                assert_eq!(forced, item.object == Object::BuildCache, "{args:?}");
                assert!(!args.iter().any(|a| a == "prune") || item.object == Object::BuildCache);
            }
        }
        let vols = commands(&Object::AnonVolumes {
            names: vec!["a".into(), "b".into()],
        });
        assert_eq!(vols.len(), 2);
        assert_eq!(vols[1].0, "docker:volume:b");
        assert_eq!(vols[1].1, ["volume", "rm", "b"]);
    }

    #[test]
    fn valid_name_rejects_option_like_and_shell_like_names() {
        for ok in [
            "alpine:latest",
            "ghcr.io/a/b:1.2",
            "sha256:7d08",
            HEX_A,
            "a_b-c@d",
        ] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "-f", "--force", "a b", "a;b", "$(x)", "中文"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn recheck_narrows_to_objects_that_are_still_unused() {
        let fresh = items();
        let vols = Object::AnonVolumes {
            names: vec![HEX_A.into(), HEX_C.into(), "gone".into()],
        };
        // HEX_C 現在有 container 在用、gone 已不存在：只留下 HEX_A
        assert_eq!(
            recheck(&vols, &fresh),
            Ok(Object::AnonVolumes {
                names: vec![HEX_A.into()]
            })
        );
        assert!(recheck(
            &Object::AnonVolumes {
                names: vec![HEX_C.into()]
            },
            &fresh
        )
        .is_err());

        let alpine = Object::Image {
            reference: "alpine:latest".into(),
        };
        assert_eq!(recheck(&alpine, &fresh), Ok(alpine.clone()));
        let used = Object::Image {
            reference: "odoo:18".into(),
        };
        assert!(recheck(&used, &fresh).is_err());

        let running = Object::Container {
            id: "610ea01770aa".into(),
            name: "x".into(),
        };
        assert!(recheck(&running, &fresh).is_err());
        assert!(recheck(&Object::BuildCache, &[]).is_err());
        let named = Object::NamedVolume {
            name: "odoo-demo_odoo19-db".into(),
        };
        assert_eq!(recheck(&named, &fresh), Ok(named.clone()));
        // 重查時已有 container 使用（不在清單裡）就不動
        let in_use = Object::NamedVolume {
            name: "mars_pg".into(),
        };
        assert!(recheck(&in_use, &fresh).is_err());
    }

    #[test]
    fn scan_reports_context_and_items() {
        let fake = FakeDocker::with_df(vec![Ok(sample_json())]);
        match scan(Some(Path::new("/usr/local/bin/docker")), &fake) {
            Scan::Ready { context, items } => {
                assert_eq!(context, "orbstack");
                assert_eq!(items, classify(&sample_json()).unwrap());
            }
            other => panic!("{other:?}"),
        }
        assert!(fake.removals().is_empty());
    }

    #[test]
    fn scan_is_unavailable_without_docker_or_daemon() {
        let fake = FakeDocker::default();
        assert_eq!(
            scan(None, &fake),
            Scan::Unavailable("找不到 docker 指令".into())
        );
        assert!(fake.calls.borrow().is_empty());

        let down = FakeDocker::with_df(vec![Err("Cannot connect to the Docker daemon".into())]);
        assert_eq!(
            scan(Some(Path::new("/x/docker")), &down),
            Scan::Unavailable("Docker 未啟動或無法連線".into())
        );

        let garbage = FakeDocker::with_df(vec![Ok("<html>".into())]);
        assert_eq!(
            scan(Some(Path::new("/x/docker")), &garbage),
            Scan::Unavailable("無法解析 docker 的輸出".into())
        );
    }

    #[test]
    fn system_runner_reads_large_output_and_times_out() {
        // 輸出超過 pipe 緩衝（64KB）也不會卡住
        let out = SystemDocker
            .run(
                Path::new("/bin/sh"),
                &["-c", "head -c 300000 /dev/zero | tr '\\0' x"],
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(out.len(), 300_000);

        let err = SystemDocker
            .run(Path::new("/bin/sleep"), &["5"], Duration::from_millis(100))
            .unwrap_err();
        assert!(err.contains("逾時"), "{err}");

        let err = SystemDocker
            .run(
                Path::new("/bin/sh"),
                &["-c", "echo boom >&2; exit 3"],
                Duration::from_secs(10),
            )
            .unwrap_err();
        assert_eq!(err, "boom");
    }
}
