#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libfelix::prelude::*;

#[derive(Clone, Copy, Default)]
struct CpuTimes {
    user: u32,
    system: u32,
    idle: u32,
}

#[derive(Clone, Copy, Default)]
struct MemInfo {
    total_kib: u64,
    managed_kib: u64,
    free_kib: u64,
    used_kib: u64,
}

struct ProcessRow {
    pid: i32,
    ppid: i32,
    cpu: i32,
    state: char,
    name: String,
}

fn read_cpu_snapshot() -> Result<Vec<CpuTimes>, ()> {
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
    if cpus.is_empty() { Err(()) } else { Ok(cpus) }
}

fn delta_usage(before: CpuTimes, after: CpuTimes) -> u32 {
    let user = after.user.wrapping_sub(before.user);
    let system = after.system.wrapping_sub(before.system);
    let idle = after.idle.wrapping_sub(before.idle);
    let total = user.saturating_add(system).saturating_add(idle);
    if total == 0 {
        0
    } else {
        user.saturating_add(system).saturating_mul(1000) / total
    }
}

fn read_meminfo() -> MemInfo {
    let Ok(text) = fs::read_to_string("/proc/meminfo") else {
        return MemInfo::default();
    };
    let mut info = MemInfo::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(value) = parts.next().and_then(|v| v.parse::<u64>().ok()) else { continue };
        match key.trim_end_matches(':') {
            "MemTotal" => info.total_kib = value,
            "MemManaged" => info.managed_kib = value,
            "MemFree" => info.free_kib = value,
            "MemUsed" => info.used_kib = value,
            _ => {}
        }
    }
    info
}

fn status_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name == key { Some(value.trim()) } else { None }
    })
}

fn read_processes() -> Vec<ProcessRow> {
    let mut rows = Vec::new();
    let Ok(entries) = fs::read_dir_entries("/proc") else {
        return rows;
    };
    for entry in entries {
        let Ok(pid) = entry.name.parse::<i32>() else { continue };
        let path = format!("/proc/{}/status", pid);
        let Ok(status) = fs::read_to_string(&path) else { continue };
        let name = status_value(&status, "Name").unwrap_or("?").to_string();
        let ppid = status_value(&status, "PPid")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(-1);
        let cpu = status_value(&status, "Cpu")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(-1);
        let state = status_value(&status, "State")
            .and_then(|v| v.chars().next())
            .unwrap_or('?');
        rows.push(ProcessRow { pid, ppid, cpu, state, name });
    }
    rows.sort_by_key(|row| row.pid);
    rows
}

fn bar(value_tenths: u32, width: usize) -> String {
    let filled = ((value_tenths.min(1000) as usize * width) / 1000).min(width);
    let mut out = String::with_capacity(width + 2);
    out.push('[');
    for i in 0..width {
        out.push(if i < filled { '#' } else { '-' });
    }
    out.push(']');
    out
}

fn mib(kib: u64) -> u64 {
    kib / 1024
}

fn draw(previous: &[CpuTimes], current: &[CpuTimes]) {
    let count = previous.len().min(current.len());
    let mut loads = Vec::with_capacity(count);
    let mut busy_sum = 0u64;
    for i in 0..count {
        let load = delta_usage(previous[i], current[i]);
        busy_sum += load as u64;
        loads.push(load);
    }
    let overall = if count == 0 { 0 } else { (busy_sum / count as u64) as u32 };
    let mem = read_meminfo();
    let processes = read_processes();

    print!("\x1b[2J\x1b[H");
    println!("Felix htop   CPUs: {}   Processes: {}   Ctrl+C: exit", count, processes.len());
    println!("CPU {} {:>3}.{}%", bar(overall, 32), overall / 10, overall % 10);

    for base in (0..count).step_by(8) {
        for cpu in base..core::cmp::min(base + 8, count) {
            let load = loads[cpu];
            print!(" C{:02}:{:>3}.{}%", cpu, load / 10, load % 10);
        }
        println!("");
    }

    let mem_pct = if mem.managed_kib == 0 {
        0
    } else {
        ((mem.used_kib.saturating_mul(1000)) / mem.managed_kib).min(1000) as u32
    };
    println!("");
    println!(
        "RAM installed: {} MiB   managed: {} MiB   used: {} MiB   free: {} MiB",
        mib(mem.total_kib), mib(mem.managed_kib), mib(mem.used_kib), mib(mem.free_kib)
    );
    println!("MEM {} {:>3}.{}% of managed", bar(mem_pct, 32), mem_pct / 10, mem_pct % 10);
    if mem.total_kib > mem.managed_kib {
        println!("     highmem not managed yet: {} MiB", mib(mem.total_kib - mem.managed_kib));
    }

    println!("");
    println!(" PID   PPID CPU S  NAME");
    println!("----- ----- --- -  ------------------------------");
    for row in processes.iter().take(24) {
        if row.cpu >= 0 {
            println!("{:>5} {:>5} {:>3} {}  {}", row.pid, row.ppid, row.cpu, row.state, row.name);
        } else {
            println!("{:>5} {:>5}   - {}  {}", row.pid, row.ppid, row.state, row.name);
        }
    }
    if processes.len() > 24 {
        println!("... {} more process(es)", processes.len() - 24);
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut previous = match read_cpu_snapshot() {
        Ok(v) => v,
        Err(_) => {
            println!("htop: cannot read /proc/stat");
            return 1;
        }
    };

    loop {
        unsafe { libfelix::syscall::sys_sleep(1000) };
        let current = match read_cpu_snapshot() {
            Ok(v) => v,
            Err(_) => {
                println!("htop: cannot read /proc/stat");
                return 1;
            }
        };
        draw(&previous, &current);
        previous = current;
    }
}
