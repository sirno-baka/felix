//! USB core: enumeration, device/interface objects, driver matching and hot-unbind.

use super::desc::{self, Config, DeviceDesc, Interface};
use super::driver;
use super::ohci::Ohci;
use crate::println;
use crate::sync::mutex::Mutex;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone)]
pub struct UsbInterface {
    pub descriptor: Interface,
    pub driver: Option<&'static str>,
}

#[derive(Clone)]
pub struct UsbDevice {
    pub controller_mmio: usize,
    pub port: u8,
    pub address: u8,
    pub descriptor: DeviceDesc,
    pub configuration: Config,
    pub device_driver: Option<&'static str>,
    pub interfaces: Vec<UsbInterface>,
}

static DEVICES: Mutex<Vec<UsbDevice>> = Mutex::new(Vec::new());

pub fn devices() -> usize {
    DEVICES.lock().len()
}

pub fn list() -> Vec<UsbDevice> {
    DEVICES.lock().clone()
}

/// Compatibility entry point used by old callers. New root-hub enumeration
/// should call bind_with_port() so disconnects can be associated with a port.
pub fn bind(hc: &Ohci, addr: u8, raw_dev: &[u8; 18]) {
    bind_with_port(hc, 0xFF, addr, raw_dev);
}

pub fn bind_with_port(hc: &Ohci, port: u8, addr: u8, raw_dev: &[u8; 18]) {
    let Some(dd) = DeviceDesc::parse(raw_dev) else {
        println!("[usb] invalid device descriptor addr={}", addr);
        return;
    };

    println!(
        "[usb] addr={} {:04x}:{:04x} class={}",
        addr,
        dd.vid,
        dd.pid,
        desc::class_name(dd.class)
    );

    let mut hdr = [0u8; 9];
    if hc
        .control(addr, &desc::get_descriptor(desc::DT_CONFIG, 0, 9), &mut hdr, true)
        .is_err()
    {
        println!("[usb] GET_DESCRIPTOR config hdr failed");
        return;
    }

    let total = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
    let total = total.clamp(9, 256);
    let mut cfg_buf = vec![0u8; total];
    if hc
        .control(
            addr,
            &desc::get_descriptor(desc::DT_CONFIG, 0, total as u16),
            &mut cfg_buf,
            true,
        )
        .is_err()
    {
        println!("[usb] GET_DESCRIPTOR config failed");
        return;
    }

    let Some(cfg) = Config::parse(&cfg_buf) else {
        println!("[usb] bad config descriptor");
        return;
    };

    let mut empty: [u8; 0] = [];
    if let Err(e) = hc.control(addr, &desc::set_configuration(cfg.value), &mut empty, false) {
        println!("[usb] SET_CONFIGURATION: {}", e);
        return;
    }

    let mut dev = UsbDevice {
        controller_mmio: hc.mmio,
        port,
        address: addr,
        descriptor: dd,
        configuration: cfg.clone(),
        device_driver: None,
        interfaces: cfg
            .interfaces
            .iter()
            .cloned()
            .map(|descriptor| UsbInterface { descriptor, driver: None })
            .collect(),
    };

    // A device-class driver (currently external USB hubs) gets one probe.
    if dd.class != desc::CLASS_PER_INTERFACE {
        if let Some((drv, score)) = driver::best_match(driver::registry(), &dd, None) {
            println!("[usb] device driver={} score={}", drv.name, score);
            match (drv.probe)(hc, addr, &dev, None) {
                Ok(()) => dev.device_driver = Some(drv.name),
                Err(e) => println!("[usb] driver {} probe: {}", drv.name, e),
            }
        }
    }

    // USB drivers normally bind to interfaces, not whole devices. If a
    // device-level driver claimed the device, it owns the device and we do not
    // also bind an interface driver to the same interfaces.
    if dev.device_driver.is_some() {
        let mut devices = DEVICES.lock();
        devices.retain(|d| !(d.controller_mmio == hc.mmio && d.port == port));
        devices.push(dev);
        return;
    }

    for i in 0..dev.interfaces.len() {
        if dev.interfaces[i].driver.is_some() {
            continue;
        }
        let iface = &dev.interfaces[i].descriptor;
        println!(
            "[usb] iface {} class={} sub=0x{:02x} proto=0x{:02x} eps={}",
            iface.number,
            desc::class_name(iface.class),
            iface.subclass,
            iface.protocol,
            iface.endpoints.len()
        );

        let Some((drv, score)) = driver::best_match(driver::registry(), &dd, Some(iface)) else {
            continue;
        };
        println!(
            "[usb] match iface={} driver={} score={}",
            iface.number, drv.name, score
        );
        match (drv.probe)(hc, addr, &dev, Some(iface)) {
            Ok(()) => dev.interfaces[i].driver = Some(drv.name),
            Err(e) => println!("[usb] driver {} probe: {}", drv.name, e),
        }
    }

    let mut devices = DEVICES.lock();
    // Do not leave a stale record if enumeration is retried for the same port.
    devices.retain(|d| !(d.controller_mmio == hc.mmio && d.port == port));
    devices.push(dev);
}

/// Disconnect a root-hub device by physical port. Driver disconnect callbacks
/// run outside the global device-list lock, so a driver can touch its own state.
pub fn disconnect_port(hc: &Ohci, port: u8) -> bool {
    let removed = {
        let mut devices = DEVICES.lock();
        let pos = devices
            .iter()
            .position(|d| d.controller_mmio == hc.mmio && d.port == port);
        pos.map(|i| devices.swap_remove(i))
    };

    let Some(dev) = removed else { return false; };

    if let Some(name) = dev.device_driver {
        if let Some(drv) = driver::registry().iter().find(|d| d.name == name) {
            (drv.disconnect)(hc, dev.address, 0xFF);
        }
    }

    for iface in dev.interfaces.iter() {
        let Some(name) = iface.driver else { continue; };
        if let Some(drv) = driver::registry().iter().find(|d| d.name == name) {
            (drv.disconnect)(hc, dev.address, iface.descriptor.number);
        }
    }

    // Class/VFS state is gone; now remove the old address from the HCD's
    // persistent endpoint lists as well. Otherwise a reconnected device gets a
    // new USB address while stale bulk EDs for the old address remain linked.
    hc.disconnect_address(dev.address);

    println!(
        "[usb] disconnected addr={} {:04x}:{:04x} port={}",
        dev.address, dev.descriptor.vid, dev.descriptor.pid, port
    );
    true
}

pub fn find(controller_mmio: usize, port: u8) -> Option<UsbDevice> {
    DEVICES
        .lock()
        .iter()
        .find(|d| d.controller_mmio == controller_mmio && d.port == port)
        .cloned()
}
