#![no_std]
#![no_main]

extern crate alloc;

use alloc::{format, string::String, vec::Vec};
use libfelix::{fs::{self, FileType, IoError}, prelude::*};
use libfelix::syscall::O_RDWR;

const SYSTEM_PATHS: &[&str] = &["kernel.bin", "init", "bin", "lib", "usr", "etc"];

fn usage() {
    println!("usage: felix-install --source <dir> --target <dir> [--device /dev/sdX]");
    println!("example: felix-install --source /mnt/usb0 --target /mnt/system --device /dev/sda");
    println!("updates system files; /home, /var and unrelated target files are preserved");
}

fn join(base: &str, relative: &str) -> String {
    if base == "/" { format!("/{}", relative.trim_start_matches('/')) }
    else { format!("{}/{}", base.trim_end_matches('/'), relative.trim_start_matches('/')) }
}

fn ensure_dir(path: &str) -> Result<(), IoError> {
    if fs::metadata(path).is_ok() { return Ok(()); }
    fs::create_dir(path)
}

fn copy_file(source: &str, target: &str) -> Result<(), IoError> {
    let data = fs::read(source)?;
    fs::write(target, &data)
}

fn copy_tree(source: &str, target: &str, copied: &mut usize, overwrite: bool) -> Result<(), IoError> {
    let metadata = fs::metadata(source)?;
    if metadata.st_mode & libfelix::syscall::S_IFMT != libfelix::syscall::S_IFDIR {
        if overwrite || fs::metadata(target).is_err() {
            copy_file(source, target)?;
            *copied += 1;
        }
        return Ok(());
    }
    ensure_dir(target)?;
    for entry in fs::read_dir_entries(source)? {
        if entry.name == "." || entry.name == ".." { continue; }
        let child_source = join(source, &entry.name);
        let child_target = join(target, &entry.name);
        match entry.file_type {
            FileType::Directory => copy_tree(&child_source, &child_target, copied, overwrite)?,
            FileType::Regular => {
                if overwrite || fs::metadata(&child_target).is_err() {
                    copy_file(&child_source, &child_target)?;
                    *copied += 1;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn install_boot(source: &str, device: &str) -> Result<(), IoError> {
    let stage1 = fs::read(&join(source, "usr/share/felix/boot.bin"))?;
    let stage2 = fs::read(&join(source, "usr/share/felix/bootloader.bin"))?;
    if stage1.len() < 446 || stage2.is_empty() || stage2.len() > 2047 * 512 {
        return Err(IoError::Other(usize::MAX));
    }
    let mut disk = File::open_flags(device, O_RDWR)?;
    // Preserve bytes 446..509: they contain the target disk's partition table.
    disk.seek(0)?;
    disk.write_all(&stage1[..446])?;
    disk.seek(510)?;
    disk.write_all(&[0x55, 0xaa])?;
    disk.seek(512)?;
    disk.write_all(&stage2)?;
    Ok(())
}

fn value<'a>(args: &'a [&str], name: &str) -> Option<&'a str> {
    args.windows(2).find(|pair| pair[0] == name).map(|pair| pair[1])
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args: Vec<&str> = args().collect();
    if args.iter().any(|arg| *arg == "-h" || *arg == "--help") { usage(); return 0; }
    let Some(source) = value(&args, "--source") else { usage(); return 1; };
    let Some(target) = value(&args, "--target") else { usage(); return 1; };
    if source.trim_end_matches('/') == target.trim_end_matches('/') {
        println!("felix-install: source and target must differ");
        return 1;
    }
    if fs::metadata(source).is_err() || ensure_dir(target).is_err() {
        println!("felix-install: source or target is unavailable");
        return 1;
    }

    let mut copied = 0usize;
    for relative in SYSTEM_PATHS {
        let src = join(source, relative);
        if fs::metadata(&src).is_err() { continue; }
        // Existing configuration is retained. New default files from /etc are
        // installed only when the target does not already contain them.
        let overwrite = *relative != "etc";
        if let Err(error) = copy_tree(&src, &join(target, relative), &mut copied, overwrite) {
            println!("felix-install: copy {} failed: {:?}", relative, error);
            return 1;
        }
    }
    if let Some(device) = value(&args, "--device") {
        if let Err(error) = install_boot(source, device) {
            println!("felix-install: bootloader install failed: {:?}", error);
            return 1;
        }
    }
    println!("felix-install: {} system files installed; user files preserved", copied);
    0
}
