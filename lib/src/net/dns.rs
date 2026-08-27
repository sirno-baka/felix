//! Простой DNS-резолвер для Felix (A-запись).
//! Использует UDP сокет + syscall sendto/recvfrom.

use alloc::string::ToString;
use core::fmt;
use core::fmt::{Display, Formatter};
use crate::syscall::{socket, connect, sendto, recvfrom, close, AF_INET, SOCK_DGRAM, IPPROTO_UDP, sys_sleep};
use crate::net::SockAddrIn;
use core::mem::size_of;
use core::net::Ipv4Addr;
use crate::println;

/// DNS-сервер QEMU user networking
const DNS_SERVER: [u8; 4] = [10, 0, 2, 3];
const DNS_PORT: u16 = 53;

/// Ошибки резолвинга
#[derive(Debug, PartialEq, Eq)]
pub enum DnsError {
    SocketFailed,
    SendFailed,
    RecvFailed,
    Timeout,
    BadResponse,
    NotFound,
    BadName,
}


/// Резолвить доменное имя в IPv4 (только A-запись).
///
/// ```ignore
/// let ip = libfelix::dns::resolve("example.com");
/// ```
pub fn resolve(name: &str) -> Result<[u8; 4], DnsError> {
    if name.is_empty() || name.len() > 253 {
        return Err(DnsError::BadName);
    }

    let sock = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP) };
    if sock == usize::MAX {
        return Err(DnsError::SocketFailed);
    }

    // Подключаем к DNS-серверу
    let dns_ip = Ipv4Addr::from(DNS_SERVER);
    let addr = SockAddrIn::new(dns_ip, DNS_PORT);
    println!("{}", dns_ip);
    let ret = unsafe {
        connect(
            sock as u32,
            &addr as *const _ as *const u8,
            size_of::<SockAddrIn>() as u32,
        )
    };
    if ret == usize::MAX {
        unsafe { close(sock as u32) };
        return Err(DnsError::SocketFailed);
    }

    // Формируем запрос
    let mut query = [0u8; 512];
    let qlen = build_query(name, &mut query).ok_or(DnsError::BadName)?;

    // Отправляем
    let sent = unsafe { sendto(sock as u32, query.as_ptr(), qlen) };
    if sent == 0 {
        unsafe { close(sock as u32) };
        return Err(DnsError::SendFailed);
    }

    // Принимаем ответ (простая блокировка — ядро вернёт 0 если пусто)
    let mut resp = [0u8; 512];
    let mut rlen = 0usize;
    // Простой спин с поллингом (до ~100 попыток)
    for _ in 0..6000 {
        rlen = unsafe { recvfrom(sock as u32, resp.as_mut_ptr(), resp.len()) };
        if rlen > 0 {
            break;
        }
        // Короткая пауза чтобы не крутить цикл
        // unsafe { sys_sleep(10) };
    }

    unsafe { close(sock as u32) };

    if rlen == 0 {
        return Err(DnsError::Timeout);
    }

    parse_response(&resp[..rlen], name)
}

// =====================================================================
// Формирование DNS-запроса
// =====================================================================

fn build_query(name: &str, buf: &mut [u8]) -> Option<usize> {
    let name_len = name.len();
    // Header(12) + name_len+2 + QTYPE(2) + QCLASS(2)
    let total = 12 + name_len + 2 + 4;
    if total > buf.len() {
        return None;
    }

    buf[..total].fill(0);

    // Header
    let id: u16 = 0x1234; // произвольный
    buf[0] = (id >> 8) as u8;
    buf[1] = (id & 0xFF) as u8;
    buf[2] = 0x01; // RD = Recursion Desired
    buf[3] = 0x00;
    // QDCOUNT = 1
    buf[4] = 0x00;
    buf[5] = 0x01;
    // ANCOUNT, NSCOUNT, ARCOUNT = 0

    // Question: QNAME (labels)
    let mut pos = 12;
    for part in name.split('.') {
        if part.is_empty() || part.len() > 63 {
            return None;
        }
        buf[pos] = part.len() as u8;
        pos += 1;
        buf[pos..pos + part.len()].copy_from_slice(part.as_bytes());
        pos += part.len();
    }
    buf[pos] = 0; // terminator
    pos += 1;

    // QTYPE = A (1)
    buf[pos] = 0x00;
    buf[pos + 1] = 0x01;
    pos += 2;
    // QCLASS = IN (1)
    buf[pos] = 0x00;
    buf[pos + 1] = 0x01;
    pos += 2;

    Some(pos)
}

// =====================================================================
// Парсинг DNS-ответа
// =====================================================================

fn parse_response(data: &[u8], name: &str) -> Result<[u8; 4], DnsError> {
    if data.len() < 12 {
        return Err(DnsError::BadResponse);
    }

    // Проверяем ID
    let id = u16::from_be_bytes([data[0], data[1]]);
    if id != 0x1234 {
        return Err(DnsError::BadResponse);
    }

    // Проверяем флаги: QR=1, RCODE=0
    let flags = u16::from_be_bytes([data[2], data[3]]);
    if flags & 0x8000 == 0 {
        return Err(DnsError::BadResponse); // не ответ
    }
    let rcode = flags & 0x000F;
    if rcode != 0 {
        return Err(DnsError::NotFound); // NXDOMAIN и т.д.
    }

    let ancount = u16::from_be_bytes([data[6], data[7]]);
    if ancount == 0 {
        return Err(DnsError::NotFound);
    }

    // Пропускаем question секцию
    let mut pos = 12;
    pos = skip_name(data, pos)?;
    pos += 4; // QTYPE + QCLASS

    // Ищем A-запись
    for _ in 0..ancount {
        pos = skip_name(data, pos)?;

        if pos + 10 > data.len() {
            return Err(DnsError::BadResponse);
        }

        let rtype = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let rclass = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
        let rdlength = u16::from_be_bytes([data[pos + 8], data[pos + 9]]);
        pos += 10;

        if rtype == 1 && rclass == 1 && rdlength == 4 {
            // A record!
            if pos + 4 > data.len() {
                return Err(DnsError::BadResponse);
            }
            return Ok([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        }

        pos += rdlength as usize;
    }

    Err(DnsError::NotFound)
}

/// Пропускает DNS-имя (с учётом компрессии указателей).
fn skip_name(data: &[u8], mut pos: usize) -> Result<usize, DnsError> {
    let mut jumps = 0;
    loop {
        if pos >= data.len() || jumps > 10 {
            return Err(DnsError::BadResponse);
        }
        let len = data[pos];
        if len == 0 {
            return Ok(pos + 1);
        }
        if len & 0xC0 == 0xC0 {
            // Указатель компрессии — 2 байта, и имя заканчивается
            return Ok(pos + 2);
        }
        pos += 1 + len as usize;
        jumps += 1;
    }
}