use crate::io::{inb, outb};
use alloc::string::String;
use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};

#[unsafe(no_mangle)]
static JIFFIES: AtomicUsize = AtomicUsize::new(0);

// Number of ms between each irq0
// This value should be written only onth at the boot
#[unsafe(no_mangle)]
pub static mut SYSTEM_FRACTION: f64 = 1.0;

pub struct Time {
    pub second: usize,
    pub millisecond: usize,
}

impl Time {
    pub fn as_f64(&self) -> f64 {
        (self.second as f64 * 1000.0) + (self.millisecond as f64 / 1000.0)
    }
}

fn read_tsc_asm() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
        "rdtsc",
        out("eax") low,
        out("edx") high,
        options(nomem, nostack)
        );
    }
    ((high as u64) << 32) | (low as u64)
}


/// Construct a Time structure using the JIFFIES and SYSTEM_FRACTION to calculate time elapsed
/// since boot
#[inline(always)]
pub fn get_timestamp() -> Time {
    // Use the programmed PIT timebase instead of assuming a fixed TSC clock.
    // This matters on the C1MAH where CPU frequency is nowhere near the old
    // hard-coded value and PIT frequency may be changed for stability.
    let total_ms = uptime_ms() as usize;
    Time {
        second: total_ms / 1000,
        millisecond: total_ms % 1000,
    }
}

/// Increment by one the JIFFIES counter
#[inline(always)]
#[unsafe(no_mangle)]
pub fn jiffies_inc() {
    JIFFIES.fetch_add(1, Ordering::Relaxed);
}

/// Return the value stored in the JIFFIES static variable
#[inline(always)]
pub fn jiffies() -> usize {
    JIFFIES.load(Ordering::Relaxed)
}

/// Monotonic milliseconds since boot, scaled by the programmed PIT period.
#[inline]
pub fn uptime_ms() -> u64 {
    unsafe { (jiffies() as f64 * SYSTEM_FRACTION) as u64 }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RtcDateTime {
    second: u8,
    minute: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u16,
}

static RTC_BASE_EPOCH: AtomicUsize = AtomicUsize::new(0);
static RTC_BASE_UPTIME_MS: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn cmos_read(reg: u8) -> u8 {
    // Bit 7 clear keeps NMI enabled. Felix has no other CMOS user, and callers
    // reach this from syscall context with interrupts disabled.
    outb(0x70, reg & 0x7f);
    inb(0x71)
}

fn rtc_update_in_progress() -> bool {
    cmos_read(0x0a) & 0x80 != 0
}

#[inline]
fn bcd(v: u8) -> u8 {
    (v & 0x0f) + ((v >> 4) * 10)
}

fn read_rtc_once() -> Option<RtcDateTime> {
    // Avoid an infinite wait on broken/virtual hardware.
    let mut spins = 0usize;
    while rtc_update_in_progress() {
        spins += 1;
        if spins > 100_000 {
            return None;
        }
        core::hint::spin_loop();
    }

    let mut second = cmos_read(0x00);
    let mut minute = cmos_read(0x02);
    let raw_hour = cmos_read(0x04);
    let mut day = cmos_read(0x07);
    let mut month = cmos_read(0x08);
    let mut year = cmos_read(0x09);
    let status_b = cmos_read(0x0b);

    let pm = raw_hour & 0x80 != 0;
    let mut hour = raw_hour & 0x7f;
    if status_b & 0x04 == 0 {
        second = bcd(second);
        minute = bcd(minute);
        hour = bcd(hour);
        day = bcd(day);
        month = bcd(month);
        year = bcd(year);
    }

    // RTC can expose 12-hour mode; normalize it to 0..23.
    if status_b & 0x02 == 0 {
        hour %= 12;
        if pm { hour = hour.saturating_add(12); }
    }

    let year = if year >= 70 { 1900 + year as u16 } else { 2000 + year as u16 };
    if second > 59 || minute > 59 || hour > 23 || day == 0 || day > 31 || month == 0 || month > 12 {
        return None;
    }
    Some(RtcDateTime { second, minute, hour, day, month, year })
}

fn read_rtc_stable() -> Option<RtcDateTime> {
    // Read twice around an RTC second rollover and accept only a stable pair.
    for _ in 0..8 {
        let a = read_rtc_once()?;
        let b = read_rtc_once()?;
        if a == b { return Some(a); }
    }
    None
}

#[inline]
fn leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn rtc_to_unix_seconds(t: RtcDateTime) -> Option<u64> {
    if t.year < 1970 { return None; }
    const MONTH_DAYS: [u16; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut days = 0u64;
    for year in 1970..t.year {
        days += if leap_year(year) { 366 } else { 365 };
    }
    for month in 1..t.month {
        days += MONTH_DAYS[(month - 1) as usize] as u64;
        if month == 2 && leap_year(t.year) { days += 1; }
    }
    let max_day = MONTH_DAYS[(t.month - 1) as usize] + if t.month == 2 && leap_year(t.year) { 1 } else { 0 };
    if t.day as u16 > max_day { return None; }
    days += (t.day - 1) as u64;
    Some(days * 86_400 + t.hour as u64 * 3_600 + t.minute as u64 * 60 + t.second as u64)
}

/// Current UTC-ish Unix time in milliseconds. The PC RTC is treated as UTC;
/// timezone policy belongs in userspace. RTC is sampled once and PIT uptime is
/// used afterwards so reads are cheap and monotonic between CMOS updates.
pub fn realtime_ms() -> u64 {
    let mut base = RTC_BASE_EPOCH.load(Ordering::Acquire);
    if base == 0 {
        let now_up = uptime_ms() as usize;
        if let Some(epoch) = read_rtc_stable().and_then(rtc_to_unix_seconds) {
            let epoch = epoch as usize;
            RTC_BASE_UPTIME_MS.store(now_up, Ordering::Relaxed);
            RTC_BASE_EPOCH.store(epoch, Ordering::Release);
            base = epoch;
        } else {
            // RTC failure: retain deterministic uptime-based behaviour rather
            // than inventing a date. A zero base is intentionally retryable.
            return uptime_ms();
        }
    }
    let base_up = RTC_BASE_UPTIME_MS.load(Ordering::Relaxed) as u64;
    base as u64 * 1000 + uptime_ms().saturating_sub(base_up)
}

pub fn rtc_unix_seconds() -> Option<u64> {
    read_rtc_stable().and_then(rtc_to_unix_seconds)
}

/// Sleep until x millisecond have passed
pub fn sleep(ms: usize) {
    if ms > 1000 {
        // Due to interrupt frequency and time to return to this job
        // this sleep is only perform for sleep higher than a second
        sleep_ms(ms);
    } else {
        // loop over io_wait to delay
        raw_delay_ms(ms);
    }
}

/// Wait x millisecond looping over io_wait
/// This is quite imprecise but do the job
fn raw_delay_ms(ms: usize) {
    for _ in 0..(ms * 1000) {
        microsleep();
    }
}

#[inline]
fn sleep_ms(ms: usize) {
    unsafe {
        let saved_time = (JIFFIES.load(Ordering::Relaxed) as f64 * SYSTEM_FRACTION) as usize;
        while saved_time + ms > (JIFFIES.load(Ordering::Relaxed) as f64 * SYSTEM_FRACTION) as usize
        {
            crate::wrappers::sti!();
            crate::wrappers::hlt!();
            crate::wrappers::cli!();
        }
    }
}

/// Unaccurate sleep for 1 microsecond
/// io_wait should take 1~4 microsecond as stated in osdev
/// But using io_wait as a microsecond seems to be too fast
/// Slowing it down 40 times looks to do the job
/// Test where performed by listening to the mario music implemented
/// This is surely imprecise but gives a raw idea if you're timing are too fast or slow
///
///
///
#[inline]
#[allow(dead_code)]
pub fn microsleep() {
    for _ in 0..40 {
        // io wait ~1-4 microseconds
        outb(0x80, 0)
    }
}

macro_rules! isleap {
    ($arg: tt) => {
        (($arg % 4) == 0 && ($arg % 100) != 0) || (($arg % 400) == 0)
    };
}
//#define isleap(y) ((((y) % 4) == 0 && ((y) % 100) != 0) || ((y) % 400) == 0)
const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHCNT: [u8; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

// This implementation seems incorrect
pub fn ctime(mut timestamp: u32) -> String {
    let ss = timestamp % 60;
    timestamp /= 60; // minutes
    let mm = timestamp % 60;
    timestamp /= 60; // hours
    let hh = timestamp % 24;
    timestamp /= 24; // days
    let wday = (4 + timestamp) % 7; // weekday, 'twas thursday when time started

    let mut year = 1970;
    while timestamp >= 365 {
        timestamp = timestamp
            - match isleap!(year) {
                true => 366,
                false => 365,
            };
        // timestamp -= ? 366: 365;
        year = year + 1;
    }

    timestamp = timestamp + 1; // days are 1-based

    let mut month = 0;
    while timestamp > MONTHCNT[month] as u32 {
        timestamp = timestamp - MONTHCNT[month] as u32;
        month = month + 1;
    }

    if month > 2 && isleap!(year) {
        timestamp = timestamp - 1;
    }
    let date = crate::alloc::format!(
        "{} {}{:3} {:02}:{:02}:{:02} {}",
        WDAYS[wday as usize],
        MONTHS[month],
        timestamp,
        hh,
        mm,
        ss,
        year
    );
    // snprintf(buf, sizeof buf, "%s %s%3d %02d:%02d:%02d %d\n",
    // ((wday  < 0 || wday  >=  7)? "???": wdays[wday]),
    // ((month < 0 || month >= 12)? "???": months[month]),
    // (int)timestamp, hh, mm, ss, year);
    return date;
}
