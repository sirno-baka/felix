use std::io::{self, Write};

use felix_rhai::{base_scope, evaluate_file, evaluate_source, runtime_error, system_engine};
use rhai::{Array, Dynamic, EvalAltResult};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let code = match run() { Ok(code) => code, Err(error) => { eprintln!("rhai: {error}"); 1 } };
    std::process::exit(code);
}

fn run() -> Result<i32, Box<EvalAltResult>> {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_else(|| String::from("rhai"));
    let first = args.next();
    match first.as_deref() {
        Some("-h" | "--help") => { print_help(&program); return Ok(0); }
        Some("-V" | "--version") => { println!("Felix Rhai {VERSION}"); return Ok(0); }
        _ => {}
    }
    let engine = system_engine();
    match first {
        None => repl(&engine),
        Some(flag) if flag == "-e" || flag == "--eval" => {
            let Some(source) = args.next() else { return Err(runtime_error("--eval requires source text")); };
            evaluate_source(&engine, &program, &source, args.map(Dynamic::from).collect::<Array>())
        }
        Some(path) => evaluate_file(&engine, &program, path, args.map(Dynamic::from).collect::<Array>()),
    }
}

fn repl(engine: &rhai::Engine) -> Result<i32, Box<EvalAltResult>> {
    println!("Felix Rhai {VERSION}. Type :help or :quit.");
    let mut scope = base_scope("rhai", Array::new());
    loop {
        print!("> "); let _ = io::stdout().flush();
        let mut line = String::new();
        match io::stdin().read_line(&mut line) { Ok(0) => break, Ok(_) => {}, Err(error) => return Err(runtime_error(error)) }
        let line = line.trim();
        match line { "" => continue, ":q" | ":quit" | "exit" => break, ":h" | ":help" => { println!("Enter Rhai code. Commands: :help, :quit"); continue; }, _ => {} }
        match engine.eval_with_scope::<Dynamic>(&mut scope, line) {
            Ok(value) if !value.is_unit() => println!("{value:?}"), Ok(_) => {}, Err(error) => eprintln!("{error}"),
        }
    }
    Ok(0)
}

fn print_help(program: &str) {
    println!("Usage:\n  {program} <script.rhai> [args...]\n  {program} -e <source> [args...]\n  {program}");
    println!("Types: int, bool, string, array, map, blob, Window, event map");
    println!("System: filesystem, process, environment, sleep, window/drawing/event API");
}
