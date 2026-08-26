use super::types::*;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    Closed,
    Created,
    Bound,
    Listening,
    Connecting,
    Connected,
}

#[derive(Debug)]
pub struct Socket {
    pub id: usize,
    pub domain: u16,
    pub ty: u16,
    pub protocol: u8,
    pub state: SocketState,
    pub local_addr: Option<SockAddrIn>,
    pub peer_addr: Option<SockAddrIn>,
    pub backlog: usize,
    pub accept_queue: VecDeque<usize>,
    pub rx_buf: Vec<u8>,
    pub tx_buf: Vec<u8>,
    pub owner: usize, // task slot
}

impl Socket {
    pub fn new(id: usize, domain: u16, ty: u16, protocol: u8, owner: usize) -> Self {
        Self {
            id,
            domain,
            ty,
            protocol,
            state: SocketState::Created,
            local_addr: None,
            peer_addr: None,
            backlog: 0,
            accept_queue: VecDeque::new(),
            rx_buf: Vec::new(),
            tx_buf: Vec::new(),
            owner,
        }
    }
}

pub struct SocketTable {
    pub(crate) sockets: Vec<Option<Socket>>,
    pub(crate) next_id: usize,
}

impl SocketTable {
    pub const fn new() -> Self {
        Self {
            sockets: Vec::new(),
            next_id: 1, // 0 оставляем "невалидным"
        }
    }

    pub fn alloc(&mut self, domain: u16, ty: u16, protocol: u8, owner: usize) -> Option<usize> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        // переиспользуем свободные слоты
        if let Some(idx) = self.sockets.iter().position(|s| s.is_none()) {
            self.sockets[idx] = Some(Socket::new(id, domain, ty, protocol, owner));
            return Some(id);
        }

        self.sockets
            .push(Some(Socket::new(id, domain, ty, protocol, owner)));
        Some(id)
    }

    /// Зарегистрировать сокет с уже выделенным id (из NET_STACK),
    /// чтобы id в fd и в SOCKET_TABLE совпадали.
    pub fn insert_with_id(
        &mut self,
        id: usize,
        domain: u16,
        ty: u16,
        protocol: u8,
        owner: usize,
    ) -> bool {
        if id == 0 {
            return false;
        }
        // расширяем Vec до нужного размера без Clone
        while self.sockets.len() < id {
            self.sockets.push(None);
        }
        let idx = id - 1;
        if self.sockets[idx].is_some() {
            return false; // слот занят
        }
        self.sockets[idx] = Some(Socket::new(id, domain, ty, protocol, owner));
        if id >= self.next_id {
            self.next_id = id + 1;
        }
        true
    }

    pub fn get(&self, id: usize) -> Option<&Socket> {
        self.sockets
            .iter()
            .find_map(|s| s.as_ref().filter(|sock| sock.id == id))
    }

    pub fn get_mut(&mut self, id: usize) -> Option<&mut Socket> {
        self.sockets
            .iter_mut()
            .find_map(|s| s.as_mut().filter(|sock| sock.id == id))
    }

    pub fn free(&mut self, id: usize) {
        if let Some(slot) = self
            .sockets
            .iter_mut()
            .find(|s| s.as_ref().map(|sock| sock.id) == Some(id))
        {
            *slot = None;
        }
    }
}
