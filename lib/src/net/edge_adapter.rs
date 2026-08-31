//! lib/src/edge_adapter.rs
//!
//! edge-nal 0.7 adapter for Felix OS syscalls.

#![allow(async_fn_in_trait)]

use core::error::Error;
use core::fmt::{Display, Formatter};
use core::mem::size_of;
use core::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};

use embedded_io::ErrorKind;
use embedded_io_async::{ErrorType, Read, Write};
use edge_nal::{
    Close, MulticastV4, MulticastV6, Readable, TcpConnect, TcpShutdown, TcpSplit,
    UdpBind, UdpReceive, UdpSend, UdpSplit, UdpSplitMulticast,
};

use crate::syscall::{
    self, bind, close, connect, read, recvfrom, sendto, socket, write,
    AF_INET, IPPROTO_TCP, IPPROTO_UDP, POLLOUT, SOCK_DGRAM, SOCK_STREAM,
};
use crate::{async_rt, println};

// ===========================================================================
// Error type
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    SocketFailed,
    ConnectFailed,
    BindFailed,
    Io,
    Closed,
    InvalidAddress,
    Unsupported,
}

impl Error for NetError {}

impl Display for NetError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_str("")
    }
}

impl embedded_io::Error for NetError {
    fn kind(&self) -> ErrorKind {
        match self {
            NetError::Closed => ErrorKind::ConnectionReset,
            NetError::InvalidAddress => ErrorKind::InvalidInput,
            NetError::Unsupported => ErrorKind::Unsupported,
            _ => ErrorKind::Other,
        }
    }
}

// ===========================================================================
// SockAddr helpers (same as your UDP echo)
// ===========================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct InAddr {
    s_addr: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: InAddr,
    sin_zero: [u8; 8],
}


impl SockAddrIn {
    pub fn new(ip: Ipv4Addr, port: u16) -> Self {
        Self {
            sin_family: AF_INET as u16,
            sin_port: port.to_be(),
            sin_addr: InAddr {
                s_addr: u32::from_be_bytes(ip.octets()),
            },
            sin_zero: [0; 8],
        }
    }
}

fn to_v4(addr: SocketAddr) -> Result<(Ipv4Addr, u16), NetError> {
    match addr {
        SocketAddr::V4(v4) => Ok((*v4.ip(), v4.port())),
        SocketAddr::V6(_) => Err(NetError::InvalidAddress),
    }
}

// ===========================================================================
// TCP Stream
// ===========================================================================

pub struct FelixTcpStream {
    fd: u32,
}

impl Drop for FelixTcpStream {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

impl ErrorType for FelixTcpStream {
    type Error = NetError;
}

impl Read for FelixTcpStream {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            let n = unsafe { recvfrom(self.fd, buf.as_mut_ptr(), buf.len()) };
            if n == usize::MAX {
                // нет данных — ждём
                async_rt::wait_readable(self.fd).await;
            } else {
                return Ok(n); // 0 = EOF, >0 = данные
            }
        }
    }
}

impl Write for FelixTcpStream {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        loop {
            let n = unsafe { sendto(self.fd, buf.as_ptr(), buf.len()) };
            if n == usize::MAX {
                // буфер полон — ждём
                let mut pfd = syscall::PollFd {
                    fd: self.fd as i32,
                    events: POLLOUT,
                    revents: 0,
                };
                unsafe { syscall::poll(&mut pfd, 1, 0) };
                async_rt::yield_now().await;
            } else {
                return Ok(n);
            }
        }
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Readable for FelixTcpStream {
    async fn readable(&mut self) -> Result<(), Self::Error> {
        async_rt::wait_readable(self.fd).await;
        Ok(())
    }
}

impl TcpShutdown for FelixTcpStream {
    async fn close(&mut self, _what: Close) -> Result<(), Self::Error> {
        unsafe { close(self.fd) };
        Ok(())
    }

    async fn abort(&mut self) -> Result<(), Self::Error> {
        unsafe { close(self.fd) };
        Ok(())
    }
}

// --- TCP split halves (just fd copies, no Drop) ---

pub struct FelixTcpReadHalf {
    fd: u32,
}

pub struct FelixTcpWriteHalf {
    fd: u32,
}

impl ErrorType for FelixTcpReadHalf {
    type Error = NetError;
}

impl Read for FelixTcpReadHalf {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        loop {
            // TCP must use recvfrom, not SYS_READ (file path returns 0 → fake EOF).
            let n = unsafe { recvfrom(self.fd, buf.as_mut_ptr(), buf.len()) };
            if n == usize::MAX {
                async_rt::wait_readable(self.fd).await;
            } else {
                return Ok(n);
            }
        }
    }
}

impl Readable for FelixTcpReadHalf {
    async fn readable(&mut self) -> Result<(), Self::Error> {
        async_rt::wait_readable(self.fd).await;
        Ok(())
    }
}

impl ErrorType for FelixTcpWriteHalf {
    type Error = NetError;
}

impl Write for FelixTcpWriteHalf {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        loop {
            let n = unsafe { sendto(self.fd, buf.as_ptr(), buf.len()) };
            if n == usize::MAX {
                let mut pfd = syscall::PollFd {
                    fd: self.fd as i32,
                    events: POLLOUT,
                    revents: 0,
                };
                unsafe { syscall::poll(&mut pfd, 1, 0) };
                async_rt::yield_now().await;
            } else {
                return Ok(n);
            }
        }
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl TcpSplit for FelixTcpStream {
    type Read<'a> = FelixTcpReadHalf;
    type Write<'a> = FelixTcpWriteHalf;

    fn split(&mut self) -> (Self::Read<'_>, Self::Write<'_>) {
        (
            FelixTcpReadHalf { fd: self.fd },
            FelixTcpWriteHalf { fd: self.fd },
        )
    }
}

// ===========================================================================
// UDP Socket
// ===========================================================================

pub struct FelixUdpSocket {
    fd: u32,
}

impl Drop for FelixUdpSocket {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

impl ErrorType for FelixUdpSocket {
    type Error = NetError;
}

impl UdpSend for FelixUdpSocket {
    async fn send(&mut self, remote: SocketAddr, data: &[u8]) -> Result<(), Self::Error> {
        let (ip, port) = to_v4(remote)?;
        let sockaddr = SockAddrIn::new(ip, port);
        // connected-UDP: update peer then sendto
        unsafe {
            connect(
                self.fd,
                &sockaddr as *const _ as *const u8,
                size_of::<SockAddrIn>() as u32,
            );
        };
        let n = unsafe { sendto(self.fd, data.as_ptr(), data.len()) };
        if n == usize::MAX {
            Err(NetError::Io)
        } else {
            Ok(())
        }
    }
}

impl UdpReceive for FelixUdpSocket {
    async fn receive(&mut self, buffer: &mut [u8]) -> Result<(usize, SocketAddr), Self::Error> {
        loop {
            let n = unsafe { recvfrom(self.fd, buffer.as_mut_ptr(), buffer.len()) };
            if n == usize::MAX {
                async_rt::wait_readable(self.fd).await;
            } else {
                let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
                return Ok((n, addr));
            }
        }
    }
}

impl Readable for FelixUdpSocket {
    async fn readable(&mut self) -> Result<(), Self::Error> {
        async_rt::wait_readable(self.fd).await;
        Ok(())
    }
}

impl MulticastV4 for FelixUdpSocket {
    async fn join_v4(&mut self, _mc: Ipv4Addr, _if: Ipv4Addr) -> Result<(), Self::Error> {
        Err(NetError::Unsupported)
    }
    async fn leave_v4(&mut self, _mc: Ipv4Addr, _if: Ipv4Addr) -> Result<(), Self::Error> {
        Err(NetError::Unsupported)
    }
}

impl MulticastV6 for FelixUdpSocket {
    async fn join_v6(&mut self, _mc: Ipv6Addr, _if: u32) -> Result<(), Self::Error> {
        Err(NetError::Unsupported)
    }
    async fn leave_v6(&mut self, _mc: Ipv6Addr, _if: u32) -> Result<(), Self::Error> {
        Err(NetError::Unsupported)
    }
}

// --- UDP split halves ---

pub struct FelixUdpRecvHalf { fd: u32 }
pub struct FelixUdpSendHalf { fd: u32 }
pub struct FelixUdpMcastV4Half { fd: u32 }
pub struct FelixUdpMcastV6Half { fd: u32 }

impl ErrorType for FelixUdpRecvHalf { type Error = NetError; }
impl ErrorType for FelixUdpSendHalf { type Error = NetError; }
impl ErrorType for FelixUdpMcastV4Half { type Error = NetError; }
impl ErrorType for FelixUdpMcastV6Half { type Error = NetError; }

impl UdpReceive for FelixUdpRecvHalf {
    async fn receive(&mut self, buffer: &mut [u8]) -> Result<(usize, SocketAddr), Self::Error> {
        loop {
            let n = unsafe { recvfrom(self.fd, buffer.as_mut_ptr(), buffer.len()) };
            if n == usize::MAX {
                async_rt::wait_readable(self.fd).await;
            } else {
                let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
                return Ok((n, addr));
            }
        }
    }
}

impl Readable for FelixUdpRecvHalf {
    async fn readable(&mut self) -> Result<(), Self::Error> {
        async_rt::wait_readable(self.fd).await;
        Ok(())
    }
}

impl UdpSend for FelixUdpSendHalf {
    async fn send(&mut self, remote: SocketAddr, data: &[u8]) -> Result<(), Self::Error> {
        let (ip, port) = to_v4(remote)?;
        let sockaddr = SockAddrIn::new(ip, port);
        unsafe {
            connect(self.fd, &sockaddr as *const _ as *const u8, size_of::<SockAddrIn>() as u32);
        };
        let n = unsafe { sendto(self.fd, data.as_ptr(), data.len()) };
        if n == usize::MAX { Err(NetError::Io) } else { Ok(()) }
    }
}

impl MulticastV4 for FelixUdpMcastV4Half {
    async fn join_v4(&mut self, _: Ipv4Addr, _: Ipv4Addr) -> Result<(), Self::Error> {
        Err(NetError::Unsupported)
    }
    async fn leave_v4(&mut self, _: Ipv4Addr, _: Ipv4Addr) -> Result<(), Self::Error> {
        Err(NetError::Unsupported)
    }
}

impl MulticastV6 for FelixUdpMcastV6Half {
    async fn join_v6(&mut self, _: Ipv6Addr, _: u32) -> Result<(), Self::Error> {
        Err(NetError::Unsupported)
    }
    async fn leave_v6(&mut self, _: Ipv6Addr, _: u32) -> Result<(), Self::Error> {
        Err(NetError::Unsupported)
    }
}

impl UdpSplit for FelixUdpSocket {
    type Receive<'a> = FelixUdpRecvHalf;
    type Send<'a> = FelixUdpSendHalf;

    fn split(&mut self) -> (Self::Receive<'_>, Self::Send<'_>) {
        (
            FelixUdpRecvHalf { fd: self.fd },
            FelixUdpSendHalf { fd: self.fd },
        )
    }
}

impl UdpSplitMulticast for FelixUdpSocket {
    type MulticastV4<'a> = FelixUdpMcastV4Half;
    type MulticastV6<'a> = FelixUdpMcastV6Half;

    fn split_multicast(
        &mut self,
    ) -> (Self::Receive<'_>, Self::Send<'_>, Self::MulticastV4<'_>, Self::MulticastV6<'_>) {
        (
            FelixUdpRecvHalf { fd: self.fd },
            FelixUdpSendHalf { fd: self.fd },
            FelixUdpMcastV4Half { fd: self.fd },
            FelixUdpMcastV6Half { fd: self.fd },
        )
    }
}

// ===========================================================================
// Stack (factory)
// ===========================================================================
#[derive(Clone)]
pub struct FelixStack;

impl TcpConnect for FelixStack {
    type Error = NetError;
    type Socket<'a> = FelixTcpStream;

    async fn connect(&self, remote: SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
        let (ip, port) = to_v4(remote)?;

        let fd = unsafe { socket(AF_INET, SOCK_STREAM, IPPROTO_TCP) };
        if fd == usize::MAX {
            return Err(NetError::SocketFailed);
        }

        let sockaddr = SockAddrIn::new(ip, port);
        let ret = unsafe {
            connect(fd as u32, &sockaddr as *const _ as *const u8, size_of::<SockAddrIn>() as u32)
        };
        if ret == usize::MAX {
            unsafe { close(fd as u32) };
            return Err(NetError::ConnectFailed);
        }

        Ok(FelixTcpStream { fd: fd as u32 })
    }
}

impl UdpBind for FelixStack {
    type Error = NetError;
    type Socket<'a> = FelixUdpSocket;

    async fn bind(&self, local: SocketAddr) -> Result<Self::Socket<'_>, Self::Error> {
        let (ip, port) = to_v4(local)?;

        let fd = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP) };
        if fd == usize::MAX {
            return Err(NetError::SocketFailed);
        }

        let sockaddr = SockAddrIn::new(ip, port);
        let ret = unsafe {
            bind(fd as u32, &sockaddr as *const _ as *const u8, size_of::<SockAddrIn>() as u32)
        };
        if ret == usize::MAX {
            unsafe { close(fd as u32) };
            return Err(NetError::BindFailed);
        }

        Ok(FelixUdpSocket { fd: fd as u32 })
    }
}