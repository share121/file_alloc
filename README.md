# file_alloc

[![GitHub last commit](https://img.shields.io/github/last-commit/share121/file_alloc/main)](https://github.com/share121/file_alloc/commits/main)
[![Test](https://github.com/share121/file_alloc/workflows/Test/badge.svg)](https://github.com/share121/file_alloc/actions)
[![Latest version](https://img.shields.io/crates/v/file_alloc.svg)](https://crates.io/crates/file_alloc)
[![Documentation](https://docs.rs/file_alloc/badge.svg)](https://docs.rs/file_alloc)
[![License](https://img.shields.io/crates/l/file_alloc.svg)](https://github.com/share121/file_alloc/blob/main/LICENSE)

跨平台、高性能、兼容性好的文件大小分配库

- **跨平台**：支持 Windows、Linux、MacOS
- **高性能**：优先使用 fallocate 高速分配
- **兼容性好**：支持自动回退到全部写 0
- **取消安全**：可用随时中断分配
- **轻量**：只依赖与 `tokio`、`rustix`、`windows-sys`
