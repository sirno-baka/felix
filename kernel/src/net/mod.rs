pub mod socket;
pub mod stack;
pub mod types;

pub use socket::*;
pub use stack::*;
pub use types::*;

use crate::sync::mutex::Mutex;
use socket::SocketTable;

pub static SOCKET_TABLE: Mutex<SocketTable> = Mutex::new(SocketTable::new());
