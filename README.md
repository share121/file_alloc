# file_alloc

跨平台、高性能、兼容性好的文件大小分配库

- **跨平台**：支持 Windows、Linux、MacOS
- **高性能**：优先使用 fallocate 高速分配
- **兼容性好**：支持自动回退到全部写 0
- **取消安全**：可用随时中断分配
- **轻量**：只依赖与 `tokio`、`rustix`、`windows-sys`
