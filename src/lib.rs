#![doc = include_str!("../README.md")]

use std::future::Future;
use std::io::{self, SeekFrom};
use tokio::fs::File;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

mod unix;
mod windows;

#[cfg(unix)]
use unix::try_fast_preallocate;
#[cfg(windows)]
use windows::try_fast_preallocate;

#[cfg(unix)]
pub use unix::init_fast_alloc;
#[cfg(windows)]
pub use windows::init_fast_alloc;
#[cfg(not(any(windows, unix)))]
pub fn init_fast_alloc() -> bool {
    false
}

pub trait FileAlloc {
    fn allocate(&mut self, size: u64) -> impl Future<Output = io::Result<()>> + Send + Sync + '_;
}

impl FileAlloc for File {
    async fn allocate(&mut self, size: u64) -> io::Result<()> {
        let current_size = self.metadata().await?.len();
        if current_size >= size || try_fast_preallocate(self, current_size, size).await? {
            Ok(())
        } else {
            async_zero_fill(self, current_size, size).await
        }
    }
}

const CHUNK_SIZE: usize = 1024 * 1024;
static ZEROS: [u8; CHUNK_SIZE] = [0; CHUNK_SIZE];

async fn async_zero_fill(
    file: &mut File,
    mut current_size: u64,
    target_size: u64,
) -> io::Result<()> {
    file.seek(SeekFrom::Start(current_size)).await?;
    while current_size < target_size {
        let remaining = target_size - current_size;
        #[allow(clippy::cast_possible_truncation)]
        let to_write = CHUNK_SIZE.min(remaining as usize);
        let n = file.write(&ZEROS[..to_write]).await?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        current_size += n as u64;
    }
    file.flush().await?;
    Ok(())
}

#[cfg(not(any(windows, unix)))]
async fn try_fast_preallocate(_file: &File, _current_size: u64, _size: u64) -> io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::NamedTempFile;

    /// 测试基础分配功能
    #[tokio::test]
    async fn test_allocate_basic() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let target_size = 5 * 1024 * 1024; // 5MB
        file.allocate(target_size).await?;

        let metadata = file.metadata().await?;
        assert_eq!(metadata.len(), target_size);

        Ok(())
    }

    /// 测试幂等性：分配比当前更小的大小不应改变文件
    #[tokio::test]
    async fn test_allocate_idempotency() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        // 先分配 2MB
        file.allocate(2 * 1024 * 1024).await?;
        let size1 = file.metadata().await?.len();

        // 尝试分配 1MB (应该直接返回 Ok)
        file.allocate(1024 * 1024).await?;
        let size2 = file.metadata().await?.len();

        assert_eq!(size1, 2 * 1024 * 1024);
        assert_eq!(size1, size2);
        Ok(())
    }

    /// 测试大文件分块分配（触发循环写 0 逻辑）
    #[tokio::test]
    async fn test_allocate_large_chunk() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        // 分配 2.5MB，超过 1MB 的 CHUNK_SIZE
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let target_size = (2.5 * 1024.0 * 1024.0) as u64;
        file.allocate(target_size).await?;

        assert_eq!(file.metadata().await?.len(), target_size);

        // 验证文件末尾是否可以写入数据
        file.seek(SeekFrom::End(0)).await?;
        file.write_all(b"end").await?;
        file.flush().await?;
        assert_eq!(file.metadata().await?.len(), target_size + 3);

        Ok(())
    }

    /// 验证分配出的空间读取出来全是 0
    #[tokio::test]
    #[cfg(not(windows))]
    async fn test_allocate_zero_verification() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let target_size = 100 * 1024; // 100KB
        file.allocate(target_size).await?;

        // 必须通过 std File 读取来验证内容
        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = Vec::new();
        std_file.read_to_end(&mut buffer)?;

        assert_eq!(buffer.len() as u64, target_size);
        assert!(buffer.iter().all(|&b| b == 0));

        Ok(())
    }

    /// 测试在已有数据的文件后面追加分配
    #[tokio::test]
    async fn test_allocate_append() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        // 先写入 10 字节数据
        let initial_data = b"0123456789";
        file.write_all(initial_data).await?;
        file.flush().await?;

        // 预分配到 100 字节
        file.allocate(100).await?;

        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = Vec::new();
        std_file.read_to_end(&mut buffer)?;

        assert_eq!(buffer.len(), 100);
        assert_eq!(&buffer[0..10], initial_data); // 原数据应保持不变
        assert!(buffer[10..].iter().all(|&b| b == 0)); // 后续应全为 0

        Ok(())
    }
}
