use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;

/// 操作紀錄（JSONL，一個目標一行，只會附加）。
///
/// 內部用 `Box<dyn Write + Send>` 而非直接存 `File`，這樣測試可以注入一個
/// 會失敗的 writer（`from_writer`），驗證「紀錄檔寫不進去就拒絕執行」的行為，
/// 而不必真的讓磁碟寫入失敗。
pub struct ActionLog {
    writer: Box<dyn Write + Send>,
}

impl ActionLog {
    pub fn open(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new().create(true).append(true).open(dir.join("actions.jsonl"))?;
        Ok(Self { writer: Box::new(file) })
    }

    /// 測試專用：包一個任意 writer（例如永遠失敗的假 writer）。
    #[cfg(test)]
    pub(crate) fn from_writer(w: impl Write + Send + 'static) -> Self {
        Self { writer: Box::new(w) }
    }

    pub fn write(&mut self, rec: &ActionRecord) -> io::Result<()> {
        let line = serde_json::to_string(rec).map_err(io::Error::other)?;
        writeln!(self.writer, "{line}")
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
