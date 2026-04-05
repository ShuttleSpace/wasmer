//! Backward-compatible shim.
//!
//! `ZipArchiveFileSystem` implementation has been merged into
//! `archive_fs.rs` to keep archive-related filesystems co-located.

pub use crate::archive_fs::ZipArchiveFileSystem;
