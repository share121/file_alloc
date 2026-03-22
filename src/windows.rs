#![cfg(windows)]

use std::{io, os::windows::io::AsRawHandle, ptr};
use tokio::fs::File;
use windows_sys::Win32::{
    Foundation::{HANDLE, LUID},
    Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueA, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    },
    Storage::FileSystem::SetFileValidData,
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub async fn try_fast_preallocate(file: &File, current_size: u64, size: u64) -> io::Result<bool> {
    file.set_len(size).await?;
    let file = file.try_clone().await?.into_std().await;
    let res = tokio::task::spawn_blocking(move || -> io::Result<bool> {
        enable_windows_privilege();
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
    .unwrap_or(Ok(false))?;
    Ok(res)
}

fn enable_windows_privilege() {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &raw mut token,
        ) == 0
        {
            return;
        }
        let mut luid: LUID = std::mem::zeroed();
        let priv_name = b"SeManageVolumePrivilege\0";
        if LookupPrivilegeValueA(ptr::null(), priv_name.as_ptr(), &raw mut luid) == 0 {
            return;
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        AdjustTokenPrivileges(token, 0, &raw const tp, 0, ptr::null_mut(), ptr::null_mut());
    }
}
