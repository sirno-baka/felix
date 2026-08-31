//! USB 1.1 host stack. Only OHCI — UHCI is not used.
//!
//! Layers:
//!   ohci     — host controller (control + bulk)
//!   desc     — descriptors / setup packets
//!   device   — bind by class
//!   hid/msc/hub — class drivers

pub mod desc;
pub mod device;
pub mod hid;
pub mod hub;
pub mod msc;
pub mod ohci;

/// Probe every PCI OHCI controller and bind class drivers.
pub fn init() {
    ohci::init_all();
    try_mount_fat();
}

/// If an MSC stick answered, mount FAT at `/mnt/usb`.
fn try_mount_fat() {
    use alloc::sync::Arc;
    use crate::disk::interface::BlockDevice;
    use crate::filesystem::fat32;
    use crate::filesystem::VFS;
    use crate::spin;

    let Some(dev) = msc::first() else {
        return;
    };
    let mut probe = [0u8; 512];
    match dev.read_sectors(1, 0, probe.as_mut_ptr() as u32) {
        Ok(()) => crate::println!(
            "[usb-msc] LBA0 sig={:02x}{:02x}",
            probe[510],
            probe[511]
        ),
        Err(e) => {
            crate::println!("[usb-msc] LBA0 read failed {}", e);
            return;
        }
    }
    let arc: Arc<spin::Mutex<dyn BlockDevice>> = Arc::new(spin::Mutex::new(dev));
    match fat32::boxed_fat(arc) {
        Some(fs) => VFS.get().mount("/mnt/usb", fs),
        None => crate::println!("[usb-msc] not FAT or mount failed"),
    }
}
