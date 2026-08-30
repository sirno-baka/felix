pub mod socket;
pub mod stack;
pub mod types;

pub use socket::*;
pub use stack::*;
pub use types::*;

use crate::sync::mutex::Mutex;
use socket::SocketTable;

pub static SOCKET_TABLE: Mutex<SocketTable> = Mutex::new(SocketTable::new());

struct NetLogger;

impl log::Log for NetLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Debug
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            crate::debugln!("[net {}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

static NET_LOGGER: NetLogger = NetLogger;

pub fn init_logger() {
    let _ = log::set_logger(&NET_LOGGER);
    log::set_max_level(log::LevelFilter::Debug);
}
