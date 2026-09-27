#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use libfelix::prelude::*;

#[derive(Clone, Copy, Default)]
struct CpuTimes {
    user: u32,
    system: u32,
    idle: u32,
}

fn read_snapshot() -> Result<Vec<CpuTimes>, ()> {
    let text = fs::read_to_string("/proc/stat").map_err(|_| ())?;
    let mut cpus = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        if name == "cpu" || !name.starts_with("cpu") {
            continue;
        }
        let values: Vec<u32> = fields.filter_map(|field| field.parse().ok()).collect();
        if values.len() < 4 {
            continue;
        }
        cpus.push(CpuTimes {
            user: values[0],
            system: values[2],
            idle: values[3],
        });
    }
    if cpus.is_empty() {
        Err(())
    } else {
        Ok(cpus)
    }
}

fn percent(part: u32, total: u32) -> u32 {
    if total == 0 {
        0
    } else {
        part.saturating_mul(1000) / total
    }
}

fn print_percent(value: u32) {
    print!("{:>3}.{}%", value / 10, value % 10);
}

fn usage_bar(busy_tenths: u32) -> String {
    let width = 24usize;
    let filled = ((busy_tenths as usize * width) / 1000).min(width);
    let mut bar = String::with_capacity(width + 2);
    bar.push('[');
    for column in 0..width {
        bar.push(if column < filled { '#' } else { '-' });
    }
    bar.push(']');
    bar
}

fn draw(previous: &[CpuTimes], current: &[CpuTimes], interval_ms: u32) {
    print!("\x1b[2J\x1b[H");
    println!(
        "Felix CPU usage  (sample: {} ms, Ctrl+C to exit)",
        interval_ms
    );
    println!("CPU       USER   SYSTEM     IDLE     BUSY  LOAD");

    for (cpu, (&before, &after)) in previous.iter().zip(current.iter()).enumerate() {
        let user = after.user.wrapping_sub(before.user);
        let system = after.system.wrapping_sub(before.system);
        let idle = after.idle.wrapping_sub(before.idle);
        let total = user.saturating_add(system).saturating_add(idle);
        let user_pct = percent(user, total);
        let system_pct = percent(system, total);
        let idle_pct = percent(idle, total);
        let busy_pct = 1000u32.saturating_sub(idle_pct);

        print!("cpu{:<2}  ", cpu);
        print_percent(user_pct);
        print!("  ");
        print_percent(system_pct);
        print!("  ");
        print_percent(idle_pct);
        print!("  ");
        print_percent(busy_pct);
        println!("  {}", usage_bar(busy_pct));
    }
}

fn parse_interval_ms() -> u32 {
    let mut values = args();
    let _program = values.next();
    values
        .next()
        .and_then(|arg| arg.parse::<u32>().ok())
        .unwrap_or(1000)
        .clamp(100, 60_000)
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let interval_ms = parse_interval_ms();
    let mut previous = match read_snapshot() {
        Ok(snapshot) => snapshot,
        Err(_) => {
            println!("cpustat: cannot read /proc/stat");
            return 1;
        }
    };

    loop {
        unsafe { libfelix::syscall::sys_sleep(interval_ms) };
        let current = match read_snapshot() {
            Ok(snapshot) => snapshot,
            Err(_) => {
                println!("cpustat: cannot read /proc/stat");
                return 1;
            }
        };
        draw(&previous, &current, interval_ms);
        previous = current;
    }
}
