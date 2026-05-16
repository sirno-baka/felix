use spin::Mutex;
use crate::filesystem::ext2::Ext2;

pub mod ext2;
pub mod vfs;           // ← новый модуль

pub use vfs::{Filesystem, VFS};
