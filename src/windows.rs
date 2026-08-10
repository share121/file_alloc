#![cfg(windows)]

use std::{io, os::windows::io::AsRawHandle, ptr, sync::OnceLock};
use tokio::fs::File;
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_SUCCESS, HANDLE, LUID},
    Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueA, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    },
    Storage::FileSystem::SetFileValidData,
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

static PRIVILEGE_ENABLED: OnceLock<bool> = OnceLock::new();

/// 初始化 Windows 快速分配权限
/// 必须在打开文件 (`File::open`) 之前调用此函数
/// 返回 true 表示提权成功（或者已成功），返回 false 表示提权失败（通常是因为没有管理员权限）
pub fn init_fast_alloc() -> bool {
    *PRIVILEGE_ENABLED.get_or_init(|| unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &raw mut token,
        ) == 0
        {
            return false;
        }
        let mut luid: LUID = std::mem::zeroed();
        let priv_name = b"SeManageVolumePrivilege\0";
        if LookupPrivilegeValueA(ptr::null(), priv_name.as_ptr(), &raw mut luid) == 0 {
            CloseHandle(token);
            return false;
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let success =
            AdjustTokenPrivileges(token, 0, &raw const tp, 0, ptr::null_mut(), ptr::null_mut())
                != 0
                && GetLastError() == ERROR_SUCCESS;
        CloseHandle(token);
        success
    })
}

pub async fn try_fast_preallocate(file: &File, current_size: u64, size: u64) -> io::Result<bool> {
    if !init_fast_alloc() {
        return Ok(false);
    }
    let file = file.try_clone().await?.into_std().await;
    let res = tokio::task::spawn_blocking(move || -> io::Result<bool> {
        file.set_len(size)?;
        let handle = file.as_raw_handle() as HANDLE;
        let success = unsafe {
            SetFileValidData(
                handle,
                i64::try_from(size).map_err(|_| io::ErrorKind::FileTooLarge)?,
            ) != 0
        };
        if !success {
            file.set_len(current_size)?;
        }
        Ok(success)
    })
    .await
    .map_err(io::Error::other)??;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use tokio::fs::File;

    /// 初始化快分配不应出错，且多次调用返回一致的缓存值
    #[test]
    fn test_init_fast_alloc_idempotent() {
        let first = init_fast_alloc();
        assert_eq!(first, init_fast_alloc());
    }

    /// 无提权时快路径应返回失败且不改动文件
    #[tokio::test]
    async fn test_try_fast_preallocate_no_privilege() -> std::io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let result = try_fast_preallocate(&file, 0, 1024).await;
        assert!(matches!(result, Ok(false)));
        assert_eq!(std::fs::metadata(temp_file.path())?.len(), 0);
        Ok(())
    }
}
