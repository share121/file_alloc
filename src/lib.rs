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
        if current_size >= size
            || matches!(
                try_fast_preallocate(self, current_size, size).await,
                Ok(true)
            )
        {
            Ok(())
        } else {
            async_zero_fill(self, current_size, size).await
        }
    }
}

const CHUNK_SIZE: usize = 1024 * 1024;
static ZEROS: [u8; CHUNK_SIZE] = [0; CHUNK_SIZE];

/// 计算本次零填充要写入的字节数。
///
/// 32 位平台上 `usize` 只有 32 位，剩余量达到 4GiB 整数倍时直接转 `usize` 会把低 32 位
/// 截成 0，导致算出 0 字节、进而写出 0 字节触发写错误。这里用 `try_from` 在溢出时回退到
/// `usize::MAX`，再由 `min(CHUNK_SIZE)` 保证仍写满整块，避免误报写错误。
fn chunk_to_write(remaining: u64) -> usize {
    CHUNK_SIZE.min(usize::try_from(remaining).unwrap_or(usize::MAX))
}

async fn async_zero_fill(
    file: &mut File,
    mut current_size: u64,
    target_size: u64,
) -> io::Result<()> {
    file.seek(SeekFrom::Start(current_size)).await?;
    while current_size < target_size {
        let remaining = target_size - current_size;
        let to_write = chunk_to_write(remaining);
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
        let target_size = 2 * 1024 * 1024 + 512 * 1024; // 2.5 MiB
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

    /// 对空文件分配 0 字节应为安全的无操作
    #[tokio::test]
    async fn test_allocate_size_zero() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        file.allocate(0).await?;
        assert_eq!(file.metadata().await?.len(), 0);
        Ok(())
    }

    /// 分配 0 字节不应破坏已有数据
    #[tokio::test]
    async fn test_allocate_size_zero_preserves_data() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let initial = b"0123456789";
        file.write_all(initial).await?;
        file.flush().await?;

        file.allocate(0).await?;
        assert_eq!(file.metadata().await?.len(), initial.len() as u64);

        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = Vec::new();
        std_file.read_to_end(&mut buffer)?;
        assert_eq!(&buffer, initial);
        Ok(())
    }

    /// 精确相等大小分配应为幂等无操作
    #[tokio::test]
    async fn test_allocate_idempotent_exact() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let size = 1024 * 1024u64;
        file.allocate(size).await?;
        file.allocate(size).await?;
        assert_eq!(file.metadata().await?.len(), size);
        Ok(())
    }

    /// 分多步增长分配最终应达到目标大小
    #[tokio::test]
    async fn test_allocate_grow_in_steps() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        file.allocate(1024 * 1024).await?;
        file.allocate(2 * 1024 * 1024).await?;
        file.allocate(4 * 1024 * 1024).await?;
        assert_eq!(file.metadata().await?.len(), 4 * 1024 * 1024);
        Ok(())
    }

    /// 恰好单块大小边界：单块写入且内容全零
    #[tokio::test]
    async fn test_allocate_chunk_boundary() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let size = CHUNK_SIZE as u64;
        file.allocate(size).await?;
        assert_eq!(file.metadata().await?.len(), size);

        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = vec![0u8; usize::try_from(size).unwrap()];
        std_file.read_exact(&mut buffer)?;
        assert!(buffer.iter().all(|&b| b == 0));
        Ok(())
    }

    /// 超过单块大小一个字节：触发多块写入
    #[tokio::test]
    async fn test_allocate_chunk_plus_one() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let size = CHUNK_SIZE as u64 + 1;
        file.allocate(size).await?;
        assert_eq!(file.metadata().await?.len(), size);
        Ok(())
    }

    /// 极小尺寸（1 字节、100 字节）应精确分配
    #[tokio::test]
    async fn test_allocate_tiny_sizes() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        file.allocate(1).await?;
        assert_eq!(file.metadata().await?.len(), 1);
        file.allocate(100).await?;
        assert_eq!(file.metadata().await?.len(), 100);
        Ok(())
    }

    /// 分配应保留已有内容，仅把新增尾部填零
    #[tokio::test]
    async fn test_allocate_preserves_existing_content() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let initial = b"payload";
        file.write_all(initial).await?;
        file.flush().await?;

        file.allocate(1024).await?;

        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = Vec::new();
        std_file.read_to_end(&mut buffer)?;
        assert_eq!(buffer.len(), 1024);
        assert_eq!(&buffer[0..initial.len()], initial);
        assert!(buffer[initial.len()..].iter().all(|&b| b == 0));
        Ok(())
    }

    /// 分配后关闭再重新打开，文件大小应持久化
    #[tokio::test]
    async fn test_allocate_reopen_persists() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        {
            let mut file = File::options()
                .read(true)
                .write(true)
                .open(temp_file.path())
                .await?;
            file.allocate(3 * 1024 * 1024).await?;
        }
        assert_eq!(std::fs::metadata(temp_file.path())?.len(), 3 * 1024 * 1024);
        Ok(())
    }

    /// 分配后在末尾追加数据可读回
    #[tokio::test]
    async fn test_allocate_then_append_and_read() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let size = 1024u64;
        file.allocate(size).await?;
        file.seek(SeekFrom::End(0)).await?;
        file.write_all(b"end").await?;
        file.flush().await?;
        assert_eq!(file.metadata().await?.len(), size + 3);

        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = Vec::new();
        std_file.read_to_end(&mut buffer)?;
        assert_eq!(&buffer[(usize::try_from(size).unwrap())..], b"end");
        Ok(())
    }

    /// 直测零填充：从文件头填零到指定大小
    #[tokio::test]
    async fn test_zero_fill_basic() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        async_zero_fill(&mut file, 0, 4096).await?;
        assert_eq!(file.metadata().await?.len(), 4096);

        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = vec![0u8; 4096];
        std_file.read_exact(&mut buffer)?;
        assert!(buffer.iter().all(|&b| b == 0));
        Ok(())
    }

    /// 直测零填充：从指定偏移开始填零应保留前缀
    #[tokio::test]
    async fn test_zero_fill_from_offset() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        file.write_all(b"hello").await?;
        file.flush().await?;
        async_zero_fill(&mut file, 5, 100).await?;
        assert_eq!(file.metadata().await?.len(), 100);

        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = Vec::new();
        std_file.read_to_end(&mut buffer)?;
        assert_eq!(&buffer[0..5], b"hello");
        assert!(buffer[5..].iter().all(|&b| b == 0));
        Ok(())
    }

    /// 直测零填充：起止偏移相等时为无操作且不会出错
    #[tokio::test]
    async fn test_zero_fill_equal_noop() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        file.write_all(&[0u8; 100]).await?;
        file.flush().await?;
        async_zero_fill(&mut file, 100, 100).await?;
        assert_eq!(file.metadata().await?.len(), 100);
        Ok(())
    }

    /// 直测零填充：超过单块大小时需多块写入
    #[tokio::test]
    async fn test_zero_fill_large() -> io::Result<()> {
        let temp_file = NamedTempFile::new()?;
        let mut file = File::options()
            .read(true)
            .write(true)
            .open(temp_file.path())
            .await?;

        let size = (CHUNK_SIZE as u64) * 3;
        async_zero_fill(&mut file, 0, size).await?;
        assert_eq!(file.metadata().await?.len(), size);

        let mut std_file = std::fs::File::open(temp_file.path())?;
        let mut buffer = vec![0u8; usize::try_from(size).unwrap()];
        std_file.read_exact(&mut buffer)?;
        assert!(buffer.iter().all(|&b| b == 0));
        Ok(())
    }

    /// 验证 4GiB 边界下仍能算出满块大小，确保 32 位 target 不会因截断误报写错误
    #[test]
    fn chunk_to_write_4gib_boundary() {
        assert_eq!(chunk_to_write(4 * 1024 * 1024 * 1024), CHUNK_SIZE);
        assert_eq!(chunk_to_write(4 * 1024 * 1024 * 1024 + 1), CHUNK_SIZE);
        assert_eq!(chunk_to_write(1), 1);
        assert_eq!(chunk_to_write(CHUNK_SIZE as u64 - 1), CHUNK_SIZE - 1);
        assert_eq!(chunk_to_write(CHUNK_SIZE as u64), CHUNK_SIZE);
    }
}
