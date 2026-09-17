//! Dynamic process filesystem.
//!
//! Layout:
//!   /proc/uptime
//!   /proc/mounts
//!   /proc/<pid>/status
//!   /proc/<pid>/stat
//!   /proc/<pid>/cwd

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::filesystem::vfs::{DirEntry, Filesystem, Metadata, MOUNT_REGISTRY};
use crate::multitasking::task::{Task, TASK_MANAGER};

const INO_ROOT: u32 = 1;
const INO_UPTIME: u32 = 2;
const INO_MOUNTS: u32 = 3;
const PID_BASE: u32 = 0x0001_0000;
const PID_STRIDE: u32 = 8;

pub struct ProcFs;

impl ProcFs {
    pub const fn new() -> Self {
        Self
    }

    fn task_by_pid(pid: i32) -> Option<&'static Task> {
        unsafe {
            let slot = TASK_MANAGER.slot_by_pid(pid)?;
            TASK_MANAGER.tasks[slot].as_ref()
        }
    }

    fn pid_inode(pid: i32, kind: u32) -> Option<u32> {
        if pid < 0 {
            return None;
        }
        PID_BASE
            .checked_add((pid as u32).checked_mul(PID_STRIDE)?)?
            .checked_add(kind)
    }

    fn decode_pid_inode(inode: u32) -> Option<(i32, u32)> {
        if inode < PID_BASE {
            return None;
        }
        let rel = inode - PID_BASE;
        Some(((rel / PID_STRIDE) as i32, rel % PID_STRIDE))
    }

    fn state_char(task: &Task) -> char {
        if task.zombie {
            'Z'
        } else if task.stopped {
            'T'
        } else if task.running {
            'R'
        } else {
            'S'
        }
    }

    fn task_name(task: &Task) -> &str {
        let end = task
            .name
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(task.name.len());
        core::str::from_utf8(&task.name[..end]).unwrap_or("?")
    }

    fn status(task: &Task) -> Vec<u8> {
        format!(
            "Name:\t{}\nState:\t{}\nPid:\t{}\nPPid:\t{}\nPGid:\t{}\nSid:\t{}\nTty:\t{}\nCwd:\t{}\nExitCode:\t{}\n",
            Self::task_name(task),
            Self::state_char(task),
            task.pid,
            task.ppid,
            task.pgid,
            task.sid,
            task.tty_id,
            task.cwd,
            task.exit_code,
        ).into_bytes()
    }

    fn stat(task: &Task) -> Vec<u8> {
        format!(
            "{} ({}) {} {} {} {} {} {}\n",
            task.pid,
            Self::task_name(task),
            Self::state_char(task),
            task.ppid,
            task.pgid,
            task.sid,
            task.tty_id,
            task.exit_code,
        )
        .into_bytes()
    }

    fn mounts() -> Vec<u8> {
        let mounts = MOUNT_REGISTRY.get().lock();
        let mut out = String::new();
        for mp in mounts.iter() {
            if mp == "/" {
                out.push_str("rootfs / vfs rw 0 0\n");
            } else {
                out.push_str("vfs ");
                out.push_str(mp);
                out.push_str(" vfs rw 0 0\n");
            }
        }
        out.into_bytes()
    }

    fn data_for_inode(inode: u32) -> Option<Vec<u8>> {
        match inode {
            INO_UPTIME => {
                let ms = crate::time::uptime_ms();
                Some(
                    format!(
                        "{}.{:03} {}.{:03}\n",
                        ms / 1000,
                        ms % 1000,
                        ms / 1000,
                        ms % 1000
                    )
                    .into_bytes(),
                )
            }
            INO_MOUNTS => Some(Self::mounts()),
            _ => {
                let (pid, kind) = Self::decode_pid_inode(inode)?;
                let task = Self::task_by_pid(pid)?;
                match kind {
                    1 => Some(Self::status(task)),
                    2 => Some(format!("{}\n", task.cwd).into_bytes()),
                    3 => Some(Self::stat(task)),
                    _ => None,
                }
            }
        }
    }

    fn parse_path(path: &str) -> Option<u32> {
        let clean = path.trim_matches('/');
        if clean.is_empty() || clean == "." {
            return Some(INO_ROOT);
        }
        if clean == "uptime" {
            return Some(INO_UPTIME);
        }
        if clean == "mounts" {
            return Some(INO_MOUNTS);
        }

        let mut parts = clean.split('/');
        let pid = parts.next()?.parse::<i32>().ok()?;
        let task = Self::task_by_pid(pid)?;
        let _ = task;
        match parts.next() {
            None => Self::pid_inode(pid, 0),
            Some("status") if parts.next().is_none() => Self::pid_inode(pid, 1),
            Some("cwd") if parts.next().is_none() => Self::pid_inode(pid, 2),
            Some("stat") if parts.next().is_none() => Self::pid_inode(pid, 3),
            _ => None,
        }
    }
}

impl Filesystem for ProcFs {
    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let inode = Self::parse_path(path)?;
        Self::data_for_inode(inode)
    }

    fn write_file(&mut self, _path: &str, _data: &[u8]) -> bool {
        false
    }
    fn create_file(&mut self, _path: &str, _data: &[u8]) -> bool {
        false
    }
    fn remove_file(&mut self, _path: &str) -> bool {
        false
    }
    fn mkdir(&mut self, _path: &str) -> bool {
        false
    }
    fn rmdir(&mut self, _path: &str) -> bool {
        false
    }

    fn list_directory_entries(&self, path: &str) -> Option<Vec<DirEntry>> {
        let inode = Self::parse_path(path)?;
        if inode == INO_ROOT {
            let mut out = Vec::new();
            out.push(DirEntry {
                inode: INO_UPTIME,
                name: "uptime".to_string(),
                file_type: 1,
                size: 0,
            });
            out.push(DirEntry {
                inode: INO_MOUNTS,
                name: "mounts".to_string(),
                file_type: 1,
                size: 0,
            });
            unsafe {
                for (slot, task) in TASK_MANAGER.tasks.iter().enumerate() {
                    let Some(task) = task.as_ref() else {
                        continue;
                    };
                    if task.leader_slot != slot as i8 {
                        continue;
                    }
                    if task.pid < 0 {
                        continue;
                    }
                    if let Some(ino) = Self::pid_inode(task.pid, 0) {
                        out.push(DirEntry {
                            inode: ino,
                            name: task.pid.to_string(),
                            file_type: 2,
                            size: 0,
                        });
                    }
                }
            }
            return Some(out);
        }

        if let Some((pid, kind)) = Self::decode_pid_inode(inode) {
            if kind == 0 && Self::task_by_pid(pid).is_some() {
                return Some(alloc::vec![
                    DirEntry {
                        inode: Self::pid_inode(pid, 1)?,
                        name: "status".to_string(),
                        file_type: 1,
                        size: 0
                    },
                    DirEntry {
                        inode: Self::pid_inode(pid, 2)?,
                        name: "cwd".to_string(),
                        file_type: 1,
                        size: 0
                    },
                    DirEntry {
                        inode: Self::pid_inode(pid, 3)?,
                        name: "stat".to_string(),
                        file_type: 1,
                        size: 0
                    },
                ]);
            }
        }
        None
    }

    fn resolve_path(&self, path: &str) -> Option<u32> {
        Self::parse_path(path)
    }

    fn read_at(&self, inode: u32, offset: u64, buf: &mut [u8]) -> usize {
        let Some(data) = Self::data_for_inode(inode) else {
            return 0;
        };
        let off = offset as usize;
        if off >= data.len() {
            return 0;
        }
        let n = core::cmp::min(buf.len(), data.len() - off);
        buf[..n].copy_from_slice(&data[off..off + n]);
        n
    }

    fn write_at(&mut self, _inode: u32, _offset: u64, _buf: &[u8]) -> usize {
        0
    }
    fn is_mounted(&self) -> bool {
        true
    }

    fn metadata(&self, path: &str) -> Option<Metadata> {
        let inode = Self::parse_path(path)?;
        self.metadata_inode(inode)
    }

    fn metadata_inode(&self, inode: u32) -> Option<Metadata> {
        let is_dir = inode == INO_ROOT
            || Self::decode_pid_inode(inode).map_or(false, |(pid, kind)| {
                kind == 0 && Self::task_by_pid(pid).is_some()
            });
        if is_dir {
            return Some(Metadata {
                inode,
                mode: 0o040555,
                nlink: 2,
                blksize: 4096,
                ..Metadata::default()
            });
        }
        let data = Self::data_for_inode(inode)?;
        let size = data.len() as u64;
        Some(Metadata {
            inode,
            mode: 0o100444,
            nlink: 1,
            size,
            blksize: 4096,
            blocks: (size + 511) / 512,
            ..Metadata::default()
        })
    }
}
