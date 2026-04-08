#![cfg(unix)]

use rustix::fs::{fallocate, FallocateFlags};
use std::io;
use tokio::fs::File;

/// Unix 下不需要提权，提供统一接口并直接返回 true
#[allow(clippy::must_use_candidate)]
pub const fn init_fast_alloc() -> bool {
    true
}

pub async fn try_fast_preallocate(file: &File, current_size: u64, size: u64) -> io::Result<bool> {
    let std_file = file.try_clone().await?.into_std().await;
    let res = tokio::task::spawn_blocking(move || -> io::Result<bool> {
        match fallocate(
            &std_file,
            FallocateFlags::empty(),
            current_size,
            size - current_size,
        ) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    })
    .await
    .map_err(io::Error::other)??;
    Ok(res)
}
