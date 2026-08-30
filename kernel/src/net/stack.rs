use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{dhcpv4, tcp, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr};

use crate::drivers::net::i8255x::NET as I8255X_NET;
use crate::drivers::net::rtl8139::NET as RTL_NET;
use crate::drivers::net::AnyNic;
use crate::net::socket::{Socket, SocketState, SocketTable};
use crate::net::types::*;
use crate::{print, println};
use crate::sync::mutex::Mutex;

pub const IF_MODE_NONE: u32 = 0;
pub const IF_MODE_STATIC: u32 = 1;
pub const IF_MODE_DHCP: u32 = 2;

pub const IF_STATE_DOWN: u32 = 0;
pub const IF_STATE_CONFIGURING: u32 = 1;
pub const IF_STATE_UP: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IfConfigUser {
    pub mode: u32,
    pub state: u32,
    pub ip: u32,
    pub prefix: u32,
    pub gateway: u32,
    pub dns: u32,
    pub mac: [u8; 6],
    pub _pad: [u8; 2],
}

impl Default for IfConfigUser {
    fn default() -> Self {
        Self {
            mode: IF_MODE_NONE,
            state: IF_STATE_DOWN,
            ip: 0,
            prefix: 0,
            gateway: 0,
            dns: 0,
            mac: [0; 6],
            _pad: [0; 2],
        }
    }
}

pub struct NetStack {
    pub iface: Interface,
    pub device: AnyNic,
    pub sockets: SocketSet<'static>,
    /// наш socket_id → (smoltcp handle, is_tcp)
    pub handles: Vec<Option<(SocketHandle, bool)>>,
    dhcp_handle: Option<SocketHandle>,
    if_mode: u32,
    if_state: u32,
    if_ip: Ipv4Address,
    if_prefix: u8,
    if_gw: Option<Ipv4Address>,
    if_dns: Option<Ipv4Address>,
    mac: [u8; 6],
}

impl NetStack {
    pub fn new(mut device: AnyNic) -> Self {
        let mac = device.mac();
        let config = Config::new(EthernetAddress(mac).into());
        let mut iface = Interface::new(config, &mut device, Instant::from_millis(0));

        // address via ifconfig static | dhcp

        let sockets = SocketSet::new(vec![]);

        Self {
            iface,
            device,
            sockets,
            handles: Vec::new(),
            dhcp_handle: None,
            if_mode: IF_MODE_NONE,
            if_state: IF_STATE_DOWN,
            if_ip: Ipv4Address::UNSPECIFIED,
            if_prefix: 0,
            if_gw: None,
            if_dns: None,
            mac,
        }
    }

    fn clear_ipv4(&mut self) {
        self.iface.update_ip_addrs(|addrs| {
            addrs.clear();
        });
        self.iface.routes_mut().remove_default_ipv4_route();
        self.if_ip = Ipv4Address::UNSPECIFIED;
        self.if_prefix = 0;
        self.if_gw = None;
        self.if_dns = None;
        self.if_state = IF_STATE_DOWN;
    }

    fn apply_ipv4(&mut self, cidr: Ipv4Cidr, gateway: Option<Ipv4Address>, dns: Option<Ipv4Address>) {
        if self.if_ip == cidr.address()
            && self.if_prefix == cidr.prefix_len()
            && self.if_gw == gateway
            && self.if_state == IF_STATE_UP
        {
            self.if_dns = dns;
            return;
        }
        self.iface.update_ip_addrs(|addrs| {
            addrs.retain(|a| matches!(a, IpCidr::Ipv6(_)));
            let _ = addrs.push(IpCidr::Ipv4(cidr));
        });
        self.iface.routes_mut().remove_default_ipv4_route();
        if let Some(gw) = gateway {
            let _ = self.iface.routes_mut().add_default_ipv4_route(gw);
        }
        self.if_ip = cidr.address();
        self.if_prefix = cidr.prefix_len();
        self.if_gw = gateway;
        self.if_dns = dns;
        self.if_state = IF_STATE_UP;
        log::debug!(
            "if up {}/{} gw={:?} dns={:?}",
            cidr.address(),
            cidr.prefix_len(),
            gateway,
            dns
        );
    }

    pub fn set_static(&mut self, ip: Ipv4Address, prefix: u8, gateway: Option<Ipv4Address>) -> Result<(), &'static str> {
        if prefix > 32 {
            return Err("bad prefix");
        }
        if let Some(h) = self.dhcp_handle.take() {
            self.sockets.remove(h);
        }
        self.if_mode = IF_MODE_STATIC;
        let cidr = Ipv4Cidr::new(ip, prefix);
        self.apply_ipv4(cidr, gateway, None);
        Ok(())
    }

    pub fn start_dhcp(&mut self) {
        self.clear_ipv4();
        self.if_mode = IF_MODE_DHCP;
        self.if_state = IF_STATE_CONFIGURING;
        if self.dhcp_handle.is_none() {
            let sock = dhcpv4::Socket::new();
            self.dhcp_handle = Some(self.sockets.add(sock));
        } else if let Some(h) = self.dhcp_handle {
            self.sockets.get_mut::<dhcpv4::Socket>(h).reset();
        }
        log::debug!("DHCP start");
    }

    fn process_dhcp(&mut self) {
        let Some(h) = self.dhcp_handle else {
            return;
        };
        let event = self.sockets.get_mut::<dhcpv4::Socket>(h).poll();
        let (addr, router, dns) = match event {
            Some(dhcpv4::Event::Configured(cfg)) => {
                log::debug!(
                    "DHCP Configured {} gw={:?} dns={:?}",
                    cfg.address,
                    cfg.router,
                    cfg.dns_servers.first()
                );
                (Some(cfg.address), cfg.router, cfg.dns_servers.first().copied())
            }
            Some(dhcpv4::Event::Deconfigured) => {
                // First poll after Socket::new always emits this. Also lease-loss.
                // Do not strip a live IPv4 — TCP would die with
                // "source IP address no longer available".
                log::debug!(
                    "DHCP Deconfigured (keep ip={} state={})",
                    self.if_ip,
                    self.if_state
                );
                if self.if_state != IF_STATE_UP {
                    self.clear_ipv4();
                    if self.if_mode == IF_MODE_DHCP {
                        self.if_state = IF_STATE_CONFIGURING;
                    }
                }
                return;
            }
            None => return,
        };
        if let Some(addr) = addr {
            self.apply_ipv4(addr, router, dns);
            self.if_mode = IF_MODE_DHCP;
        }
    }

    pub fn snapshot(&self) -> IfConfigUser {
        IfConfigUser {
            mode: self.if_mode,
            state: self.if_state,
            ip: u32::from_be_bytes(self.if_ip.octets()),
            prefix: self.if_prefix as u32,
            gateway: self.if_gw.map(|g| u32::from_be_bytes(g.octets())).unwrap_or(0),
            dns: self.if_dns.map(|d| u32::from_be_bytes(d.octets())).unwrap_or(0),
            mac: self.mac,
            _pad: [0; 2],
        }
    }

    /// Поллим стек (вызывать из таймера / из syscalls)
    pub fn poll(&mut self, timestamp_ms: i64) {
        // print!(".");
        let ts = Instant::from_millis(timestamp_ms);
        self.iface.poll(ts, &mut self.device, &mut self.sockets);
        self.process_dhcp();
    }

    /// Создать smoltcp-сокет и вернуть (наш id, smoltcp handle)
    pub fn create_socket(
        &mut self,
        domain: u16,
        ty: u16,
        protocol: u8,
    ) -> Option<(usize, SocketHandle)> {
        if domain != AF_INET {
            return None;
        }

        let handle = match ty {
            SOCK_STREAM => {
                // TCP
                let rx = tcp::SocketBuffer::new(vec![0u8; 65536]);
                let tx = tcp::SocketBuffer::new(vec![0u8; 65536]);
                let socket = tcp::Socket::new(rx, tx);
                self.sockets.add(socket)
            }
            SOCK_DGRAM => {
                // UDP
                let rx =
                    udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 32], vec![0u8; 65536]);
                let tx =
                    udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 32], vec![0u8; 65536]);
                let socket = udp::Socket::new(rx, tx);
                self.sockets.add(socket)
            }
            _ => return None,
        };

        let is_tcp = ty == SOCK_STREAM;

        // ищем свободный слот в handles или пушим
        let id = if let Some(idx) = self.handles.iter().position(|h| h.is_none()) {
            self.handles[idx] = Some((handle, is_tcp));
            idx + 1 // 0 = invalid
        } else {
            self.handles.push(Some((handle, is_tcp)));
            self.handles.len()
        };

        Some((id, handle))
    }

    pub fn get_handle(&self, id: usize) -> Option<(SocketHandle, bool)> {
        if id == 0 || id > self.handles.len() {
            return None;
        }
        self.handles[id - 1]
    }

    pub fn remove_handle(&mut self, id: usize) {
        if id > 0 && id <= self.handles.len() {
            if let Some((handle, _)) = self.handles[id - 1].take() {
                self.sockets.remove(handle);
            }
        }
    }
}

pub static NET_STACK: Mutex<Option<NetStack>> = Mutex::new(None);

/// Инициализация (вызывать один раз из main после I8255x::init)
pub fn init() {
    let device = {
        let mut guard = I8255X_NET.lock();
        guard.take().expect("NIC not initialized")
    };
    bring_up(AnyNic::I8255x(device));
}

pub fn init_rtl8139() {
    let device = {
        let mut guard = RTL_NET.lock();
        guard.take().expect("RTL8139 not initialized")
    };
    bring_up(AnyNic::Rtl8139(device));
}

fn bring_up(device: AnyNic) {
    crate::net::init_logger();
    let stack = NetStack::new(device);
    *NET_STACK.lock() = Some(stack);
    log::debug!("stack up (no address — ifconfig static|dhcp)");
}

pub fn ifconfig_get() -> Option<IfConfigUser> {
    NET_STACK.lock().as_ref().map(|s| s.snapshot())
}

pub fn ifconfig_static(ip: Ipv4Address, prefix: u8, gw: Option<Ipv4Address>) -> Result<(), &'static str> {
    match NET_STACK.lock().as_mut() {
        Some(s) => s.set_static(ip, prefix, gw),
        None => Err("no nic"),
    }
}

pub fn ifconfig_dhcp() -> Result<(), &'static str> {
    match NET_STACK.lock().as_mut() {
        Some(s) => {
            s.start_dhcp();
            Ok(())
        }
        None => Err("no nic"),
    }
}

/// Удобный хелпер для поллинга
pub fn poll_stack(timestamp_ms: i64) {
    if let Some(ref mut stack) = *NET_STACK.lock() {
        stack.poll(timestamp_ms);
    }
}
