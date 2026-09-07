//! Real on-disk usage of a directory tree.
//!
//! "On disk" means the blocks the filesystem has actually allocated — what
//! `du` reports — not the sum of logical file lengths. The cache is many small
//! files, each of which rounds up to at least one filesystem block, so the two
//! numbers differ noticeably.

use std::fs;
use std::io;
use std::path::Path;

/// Walk `dir` recursively and return `(file count, bytes on disk)`. The total
/// includes the blocks the directories themselves occupy, so it lines up with
/// `du`.
pub fn measure(dir: &Path) -> io::Result<(u64, u64)> {
    let mut files = 0;
    let mut bytes = entry_usage(dir, &fs::symlink_metadata(dir)?)?;
    walk(dir, &mut files, &mut bytes)?;
    Ok((files, bytes))
}

fn walk(dir: &Path, files: &mut u64, bytes: &mut u64) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // `DirEntry::metadata` does not follow a symlink the entry points at,
        // so this cannot loop.
        let meta = entry.metadata()?;
        let path = entry.path();
        if meta.is_dir() {
            *bytes += entry_usage(&path, &meta)?;
            walk(&path, files, bytes)?;
        } else if meta.is_file() {
            *files += 1;
            *bytes += entry_usage(&path, &meta)?;
        }
    }
    Ok(())
}

/// Blocks-allocated size of a single entry.
#[cfg(unix)]
fn entry_usage(_path: &Path, meta: &fs::Metadata) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    // `blocks` is always counted in 512-byte units, regardless of the
    // filesystem's own block size.
    Ok(meta.blocks() * 512)
}

/// Compressed/allocated size of a single file via `GetCompressedFileSizeW`,
/// which accounts for the cluster size, NTFS compression, and sparse regions.
#[cfg(windows)]
fn entry_usage(path: &Path, meta: &fs::Metadata) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    if meta.is_dir() {
        return Ok(0);
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut high: u32 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 string living past the call,
    // and `high` is a valid, writable `u32`.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    // `INVALID_FILE_SIZE` (0xFFFFFFFF) is only an error if `GetLastError` also
    // reports one — it is otherwise a legitimate low dword.
    if low == u32::MAX && unsafe { GetLastError() } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(high) << 32) | u64::from(low))
}

/// Portable fallback: the logical length, the best a platform without a
/// blocks API can offer.
#[cfg(not(any(unix, windows)))]
fn entry_usage(_path: &Path, meta: &fs::Metadata) -> io::Result<u64> {
    Ok(meta.len())
}
