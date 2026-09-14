use std::{env, path::PathBuf, process::Command, thread, time::Duration};

use rhai::{module_resolvers::FileModuleResolver, Array, Dynamic, Engine, EvalAltResult, Position, Scope};

mod fs;
mod http;
mod ui;

pub use fs::{DirEntry, FileMetadata, FileSystem};
pub use http::{HttpClient, HttpRequest, HttpResponse, JsonCodec};
pub use ui::{UiApp, UiContainer, UiElement, UiFactory};

const SYS_GETRANDOM: u32 = 355;

fn popugos_getrandom(dest: &mut [u8]) -> Result<(), getrandom::Error> {
    let mut offset = 0;
    while offset < dest.len() {
        let result: i32;
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("eax") SYS_GETRANDOM => result,
                in("ebx") dest.as_mut_ptr().add(offset),
                in("ecx") dest.len() - offset,
                in("edx") 0u32,
                options(nostack, preserves_flags),
            );
        }
        if result <= 0 { return Err(getrandom::Error::UNSUPPORTED); }
        offset += result as usize;
    }
    Ok(())
}

getrandom::register_custom_getrandom!(popugos_getrandom);

pub fn system_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(1_000_000);
    engine.set_max_call_levels(64);
    engine.set_max_expr_depths(64, 64);
    engine.set_max_string_size(1024 * 1024);
    engine.set_max_array_size(65_536);
    engine.set_max_map_size(16_384);

    let mut modules = FileModuleResolver::new_with_path("/lib/rhai");
    modules.enable_cache(false);
    engine.set_module_resolver(modules);

    engine.register_fn("env", |name: &str| env::var(name).unwrap_or_default());
    engine.register_fn("cwd", || env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
    engine.register_fn("sleep", |milliseconds: i32| thread::sleep(Duration::from_millis(milliseconds.max(0) as u64)));
    engine.register_fn("run", |program: &str| run_command(program, Array::new()));
    engine.register_fn("run", run_command);
    fs::register(&mut engine);
    http::register(&mut engine);
    ui::register(&mut engine);
    engine
}

pub fn base_scope(program: &str, args: Array) -> Scope<'static> {
    let mut scope = Scope::new();
    scope.push_constant("OS", "PopugOS");
    scope.push_constant("PROGRAM", program.to_owned());
    scope.push_constant("ARGS", args);
    // Rhai dispatches custom-type methods through a mutable receiver even
    // when the Rust method is logically read-only, so these service objects
    // must be regular scope variables rather than constants.
    scope.push("http", HttpClient);
    scope.push("json", JsonCodec);
    scope.push("fs", FileSystem);
    scope.push("ui", UiFactory);
    scope
}

pub fn evaluate_source(engine: &Engine, program: &str, source: &str, args: Array) -> Result<i32, Box<EvalAltResult>> {
    let mut scope = base_scope(program, args);
    let result = engine.eval_with_scope::<Dynamic>(&mut scope, source)?;
    Ok(if result.is::<i32>() { result.cast::<i32>() } else { 0 })
}

pub fn evaluate_file(engine: &Engine, program: &str, path: String, args: Array) -> Result<i32, Box<EvalAltResult>> {
    let mut scope = base_scope(program, args);
    scope.push_constant("SCRIPT", path.clone());
    let result = engine.eval_file_with_scope::<Dynamic>(&mut scope, PathBuf::from(path))?;
    Ok(if result.is::<i32>() { result.cast::<i32>() } else { 0 })
}

fn run_command(program: &str, arguments: Array) -> Result<i32, Box<EvalAltResult>> {
    let mut command = Command::new(program); for argument in arguments { command.arg(argument.to_string()); }
    command.status().map(|status| status.code().unwrap_or(1)).map_err(runtime_error)
}
pub fn runtime_error(error: impl std::fmt::Display) -> Box<EvalAltResult> { EvalAltResult::ErrorRuntime(Dynamic::from(error.to_string()), Position::NONE).into() }
