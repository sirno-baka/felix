use rustls::pki_types::UnixTime;
use rustls::time_provider::TimeProvider;
use std::fmt;
use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const NTP_UNIX_OFFSET: u64 = 2_208_988_800;
const MIN_REASONABLE_UNIX: u64 = 1_704_067_200; // 2024-01-01
const MAX_REASONABLE_UNIX: u64 = 4_102_444_800; // 2100-01-01
const NTP_SERVERS: [&str; 3] = ["time.cloudflare.com:123", "time.google.com:123", "pool.ntp.org:123"];

#[derive(Debug)]
struct NetworkTime {
    unix_at_start: u64,
    started: Instant,
}

impl TimeProvider for NetworkTime {
    fn current_time(&self) -> Option<UnixTime> {
        Some(UnixTime::since_unix_epoch(Duration::from_secs(
            self.unix_at_start.saturating_add(self.started.elapsed().as_secs()),
        )))
    }
}

pub struct ResolvedTime {
    pub provider: Arc<dyn TimeProvider>,
    pub description: String,
}

impl fmt::Debug for ResolvedTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedTime").field("description", &self.description).finish()
    }
}

pub fn resolve() -> ResolvedTime {
    for server in NTP_SERVERS {
        if let Ok(seconds) = query_ntp(server) {
            return resolved(seconds, format!("NTP {server}"));
        }
    }

    if let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) {
        let seconds = duration.as_secs();
        if reasonable(seconds) {
            return resolved(seconds, "hardware RTC".to_string());
        }
        eprintln!("warning: hardware RTC has an invalid date ({seconds}); NTP unavailable");
    } else {
        eprintln!("warning: hardware RTC is before 1970; NTP unavailable");
    }

    let build_time = env!("TWITCH_RADIO_BUILD_UNIX").parse::<u64>().unwrap_or(MIN_REASONABLE_UNIX);
    eprintln!("warning: using binary build time for TLS certificate validation");
    resolved(build_time, "binary build time fallback".to_string())
}

fn resolved(seconds: u64, description: String) -> ResolvedTime {
    ResolvedTime {
        provider: Arc::new(NetworkTime { unix_at_start: seconds, started: Instant::now() }),
        description,
    }
}

fn reasonable(seconds: u64) -> bool {
    (MIN_REASONABLE_UNIX..MAX_REASONABLE_UNIX).contains(&seconds)
}

fn query_ntp(server: &str) -> Result<u64, String> {
    let address = server
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .next()
        .ok_or_else(|| "NTP server has no address".to_string())?;
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|error| error.to_string())?;
    socket.set_read_timeout(Some(Duration::from_secs(2))).map_err(|error| error.to_string())?;
    socket.set_write_timeout(Some(Duration::from_secs(2))).map_err(|error| error.to_string())?;
    socket.connect(address).map_err(|error| error.to_string())?;

    let mut request = [0u8; 48];
    request[0] = 0x23; // NTP v4, client mode.
    socket.send(&request).map_err(|error| error.to_string())?;
    let mut response = [0u8; 48];
    let length = socket.recv(&mut response).map_err(|error| error.to_string())?;
    parse_ntp(&response[..length])
}

fn parse_ntp(response: &[u8]) -> Result<u64, String> {
    if response.len() < 48 {
        return Err("short NTP response".to_string());
    }
    let mode = response[0] & 0x07;
    let stratum = response[1];
    if mode != 4 || stratum == 0 || stratum > 15 {
        return Err("invalid NTP server response".to_string());
    }
    let ntp_seconds = u32::from_be_bytes(response[40..44].try_into().unwrap()) as u64;
    let unix_seconds = ntp_seconds
        .checked_sub(NTP_UNIX_OFFSET)
        .ok_or_else(|| "NTP timestamp is before Unix epoch".to_string())?;
    if !reasonable(unix_seconds) {
        return Err("NTP returned an unreasonable date".to_string());
    }
    Ok(unix_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_ntp_transmit_time() {
        let unix = 1_789_600_000u64;
        let mut packet = [0u8; 48];
        packet[0] = 0x24;
        packet[1] = 2;
        packet[40..44].copy_from_slice(&((unix + NTP_UNIX_OFFSET) as u32).to_be_bytes());
        assert_eq!(parse_ntp(&packet).unwrap(), unix);
    }

    #[test]
    fn rejects_kiss_of_death_response() {
        let mut packet = [0u8; 48];
        packet[0] = 0x24;
        assert!(parse_ntp(&packet).is_err());
    }
}
