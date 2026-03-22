#![cfg(unix)]

use rustix::fs::{FallocateFlags, fallocate};
use std::io;
use tokio::fs::File;

pub async fn try_fast_preallocate(file: &File, _current_size: u64, size: u64) -> io::Result<bool> {
    let std_file = file.try_clone().await?.into_std().await;
    let res = tokio::task::spawn_blocking(move || -> io::Result<bool> {
        match fallocate(&std_file, FallocateFlags::empty(), 0, size) {
            Ok(_) => Ok(true),
            Err(_err) => Ok(false),
        }
    })
    .await
    .unwrap_or(Ok(false))?;
    Ok(res)
}
