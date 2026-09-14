use std::fs::{self, OpenOptions};
use std::io::Write;

use rhai::{Blob, CustomType, Engine, EvalAltResult, TypeBuilder};

use crate::runtime_error;

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "Fs", extra = Self::build_rhai_api)]
pub struct FileSystem;

impl FileSystem {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder
            .with_fn("read_text", read_text)
            .with_fn("read_bytes", read_bytes)
            .with_fn("write_text", write_text)
            .with_fn("write_bytes", write_bytes)
            .with_fn("append_text", append_text)
            .with_fn("list", list)
            .with_fn("metadata", metadata)
            .with_fn("exists", exists)
            .with_fn("is_file", is_file)
            .with_fn("is_dir", is_dir)
            .with_fn("create_dir", create_dir)
            .with_fn("remove", remove)
            .on_print(|_| "Fs".into())
            .on_debug(|_| "Fs".into());
    }
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "DirEntry")]
pub struct DirEntry {
    #[rhai_type(readonly)] pub name: String,
    #[rhai_type(readonly)] pub path: String,
    #[rhai_type(readonly)] pub is_file: bool,
    #[rhai_type(readonly)] pub is_dir: bool,
    #[rhai_type(readonly)] pub size: i32,
}

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "FileMetadata")]
pub struct FileMetadata {
    #[rhai_type(readonly)] pub is_file: bool,
    #[rhai_type(readonly)] pub is_dir: bool,
    #[rhai_type(readonly)] pub size: i32,
    #[rhai_type(readonly)] pub readonly: bool,
}

pub fn register(engine: &mut Engine) {
    engine
        .build_type::<FileSystem>()
        .build_type::<DirEntry>()
        .build_type::<FileMetadata>();
}

fn read_text(_: &mut FileSystem, path: &str) -> Result<String, Box<EvalAltResult>> {
    fs::read_to_string(path).map_err(runtime_error)
}

fn read_bytes(_: &mut FileSystem, path: &str) -> Result<Blob, Box<EvalAltResult>> {
    fs::read(path).map_err(runtime_error)
}

fn write_text(_: &mut FileSystem, path: &str, contents: &str) -> Result<(), Box<EvalAltResult>> {
    fs::write(path, contents).map_err(runtime_error)
}

fn write_bytes(_: &mut FileSystem, path: &str, contents: Blob) -> Result<(), Box<EvalAltResult>> {
    fs::write(path, contents).map_err(runtime_error)
}

fn append_text(_: &mut FileSystem, path: &str, contents: &str) -> Result<(), Box<EvalAltResult>> {
    let mut file = OpenOptions::new().create(true).append(true).open(path).map_err(runtime_error)?;
    file.write_all(contents.as_bytes()).map_err(runtime_error)
}

fn list(_: &mut FileSystem, path: &str) -> Result<Vec<DirEntry>, Box<EvalAltResult>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(path).map_err(runtime_error)? {
        let entry = entry.map_err(runtime_error)?;
        let metadata = entry.metadata().map_err(runtime_error)?;
        result.push(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path().to_string_lossy().into_owned(),
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            size: metadata.len().min(i32::MAX as u64) as i32,
        });
    }
    result.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(result)
}

fn metadata(_: &mut FileSystem, path: &str) -> Result<FileMetadata, Box<EvalAltResult>> {
    let metadata = fs::metadata(path).map_err(runtime_error)?;
    Ok(FileMetadata {
        is_file: metadata.is_file(),
        is_dir: metadata.is_dir(),
        size: metadata.len().min(i32::MAX as u64) as i32,
        readonly: metadata.permissions().readonly(),
    })
}

fn exists(_: &mut FileSystem, path: &str) -> bool { fs::metadata(path).is_ok() }
fn is_file(_: &mut FileSystem, path: &str) -> bool { fs::metadata(path).map(|m| m.is_file()).unwrap_or(false) }
fn is_dir(_: &mut FileSystem, path: &str) -> bool { fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false) }
fn create_dir(_: &mut FileSystem, path: &str) -> Result<(), Box<EvalAltResult>> { fs::create_dir_all(path).map_err(runtime_error) }

fn remove(_: &mut FileSystem, path: &str) -> Result<(), Box<EvalAltResult>> {
    let metadata = fs::metadata(path).map_err(runtime_error)?;
    if metadata.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }.map_err(runtime_error)
}
