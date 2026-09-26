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
