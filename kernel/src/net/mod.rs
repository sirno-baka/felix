pub mod types;
pub mod socket;
pub mod stack;

pub use types::*;
pub use socket::*;
pub use stack::*;

use crate::sync::mutex::Mutex;
use socket::SocketTable;

pub static SOCKET_TABLE: Mutex<SocketTable> = Mutex::new(SocketTable::new());