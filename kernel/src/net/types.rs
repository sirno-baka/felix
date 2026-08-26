#![allow(dead_code)]

pub const AF_UNSPEC: u16 = 0;
pub const AF_UNIX: u16 = 1;
pub const AF_INET: u16 = 2;

pub const SOCK_STREAM: u16 = 1;
pub const SOCK_DGRAM: u16 = 2;
pub const SOCK_RAW: u16 = 3;

pub const IPPROTO_IP: u8 = 0;
pub const IPPROTO_TCP: u8 = 6;
pub const IPPROTO_UDP: u8 = 17;

pub const SOL_SOCKET: i32 = 1;
pub const SO_REUSEADDR: i32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SockAddr {
    pub sa_family: u16,
    pub sa_data: [u8; 14],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SockAddrIn {
    pub sin_family: u16, // AF_INET
    pub sin_port: u16,   // network byte order
    pub sin_addr: InAddr,
    pub sin_zero: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InAddr {
    pub s_addr: u32, // network byte order
}

impl InAddr {
    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Self {
            s_addr: u32::from_be_bytes([a, b, c, d]),
        }
    }

    pub const LOCALHOST: InAddr = InAddr::new(127, 0, 0, 1);
    pub const ANY: InAddr = InAddr::new(0, 0, 0, 0);
}
