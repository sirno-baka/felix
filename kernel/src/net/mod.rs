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
        metadata.level() <= log::Level::Debug && metadata.target().contains("dhcp")
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            // debugln goes to the kernel log without touching the framebuffer,
            // so F12 can show the exact smoltcp DHCP state/parse decision.
            crate::debugln!(
                "[net {} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static NET_LOGGER: NetLogger = NetLogger;

pub fn init_logger() {
    let _ = log::set_logger(&NET_LOGGER);
    log::set_max_level(log::LevelFilter::Error);
}
