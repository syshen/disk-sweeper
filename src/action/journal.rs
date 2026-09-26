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
