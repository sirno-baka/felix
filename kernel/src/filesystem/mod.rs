use spin::Mutex;
use crate::filesystem::ext2::Ext2;

pub mod fat;
pub mod ext2;
pub mod vfs;           // ← новый модуль

pub use vfs::{Filesystem, VFS};

pub static EXT2_SLAVE: Mutex<Option<Ext2>> = Mutex::new(None);