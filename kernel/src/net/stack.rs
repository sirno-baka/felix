use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address};

use crate::drivers::net::i8255x::{I8255x, NET};
use crate::sync::mutex::Mutex;
use crate::net::types::*;
use crate::net::socket::{Socket, SocketState, SocketTable};
use crate::println;

pub struct NetStack {
    pub iface: Interface,
    pub device: I8255x,
    pub sockets: SocketSet<'static>,
    /// наш socket_id → (smoltcp handle, is_tcp)
    pub handles: Vec<Option<(SocketHandle, bool)>>,
}

impl NetStack {
    pub fn new(mut device: I8255x) -> Self {
        let mac = device.mac();
        let config = Config::new(EthernetAddress(mac).into());
        let mut iface = Interface::new(config, &mut device, Instant::from_millis(0));

        // QEMU user networking
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24))
                .unwrap();
        });

        iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2))
            .unwrap();

        let sockets = SocketSet::new(vec![]);

        Self {
            iface,
            device,
            sockets,
            handles: Vec::new(),
        }
    }

    /// Поллим стек (вызывать из таймера / из syscalls)
    pub fn poll(&mut self, timestamp_ms: i64) {
        let ts = Instant::from_millis(timestamp_ms);
        self.iface.poll(ts, &mut self.device, &mut self.sockets);
    }

    /// Создать smoltcp-сокет и вернуть (наш id, smoltcp handle)
    pub fn create_socket(&mut self, domain: u16, ty: u16, protocol: u8) -> Option<(usize, SocketHandle)> {
        if domain != AF_INET {
            return None;
        }

        let handle = match ty {
            SOCK_STREAM => {
                // TCP
                let rx = tcp::SocketBuffer::new(vec![0u8; 8192]);
                let tx = tcp::SocketBuffer::new(vec![0u8; 8192]);
                let socket = tcp::Socket::new(rx, tx);
                self.sockets.add(socket)
            }
            SOCK_DGRAM => {
                // UDP
                let rx = udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY; 16],
                    vec![0u8; 8192],
                );
                let tx = udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY; 16],
                    vec![0u8; 8192],
                );
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
        let mut guard = NET.lock();
        guard.take().expect("NIC not initialized")
    };

    let stack = NetStack::new(device);
    *NET_STACK.lock() = Some(stack);

    println!("net: smoltcp stack initialized (10.0.2.15/24)");
}

/// Удобный хелпер для поллинга
pub fn poll_stack(timestamp_ms: i64) {
    if let Some(ref mut stack) = *NET_STACK.lock() {
        stack.poll(timestamp_ms);
    }
}