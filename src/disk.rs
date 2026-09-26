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
