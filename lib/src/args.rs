//! Command-line argument parser.
//!
//! ```text
//! program pos1 pos2 -f --foo --name value --key=val -abc -- file
//! ```
//!
//! ```no_run
//! use libfelix::prelude::*;
//!
//! let args = Args::parse();
//! let pos1 = args.get(0);
//! if args.has("f") || args.has("foo") { /* ... */ }
//! let name = args.value("name");
//! ```

use alloc::vec::Vec;

use crate::rt;

/// Parsed command line.
#[derive(Debug, Clone)]
pub struct Args {
    program: &'static str,
    positionals: Vec<&'static str>,
    options: Vec<Opt>,
}

#[derive(Debug, Clone, Copy)]
struct Opt {
    name: &'static str,
    value: Option<&'static str>,
}

impl Args {
    /// Parse `argv` of the current process.
    pub fn parse() -> Self {
        let raw: Vec<&'static str> = rt::args().collect();
        parse_slice(&raw, &[])
    }

    /// Like `parse`, but listed short/long names consume the next token as a value.
    ///
    /// `Args::parse_valued(&["d", "data", "o"])` makes `-d BODY` and `-o FILE` work.
    pub fn parse_valued(valued: &[&str]) -> Self {
        let raw: Vec<&'static str> = rt::args().collect();
        parse_slice(&raw, valued)
    }

    /// Parse a ready list (`argv[0]` is the program name).
    pub fn parse_from(raw: &[&'static str]) -> Self {
        parse_slice(raw, &[])
    }

    pub fn parse_from_valued(raw: &[&'static str], valued: &[&str]) -> Self {
        parse_slice(raw, valued)
    }

    pub fn program(&self) -> &'static str {
        self.program
    }

    /// Positional argument `i` (0 = first after the program name).
    pub fn get(&self, i: usize) -> Option<&'static str> {
        self.positionals.get(i).copied()
    }

    pub fn positionals(&self) -> &[&'static str] {
        &self.positionals
    }

    pub fn len(&self) -> usize {
        self.positionals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positionals.is_empty()
    }

    /// True if `-x` / `--x` was present. `has("f")` and `has("foo")` both work.
    pub fn has(&self, name: &str) -> bool {
        let name = strip_dashes(name);
        self.options.iter().any(|o| o.name == name)
    }

    /// Value of `--name value`, `--name=value` or `-n value`.
    pub fn value(&self, name: &str) -> Option<&'static str> {
        let name = strip_dashes(name);
        self.options
            .iter()
            .rev()
            .find(|o| o.name == name)
            .and_then(|o| o.value)
    }

    /// All values for a repeated option.
    pub fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'static str> + 'a {
        let name = strip_dashes(name);
        self.options
            .iter()
            .filter(move |o| o.name == name)
            .filter_map(|o| o.value)
    }
}

fn strip_dashes(name: &str) -> &str {
    name.trim_start_matches('-')
}

fn looks_like_flag(s: &str) -> bool {
    if s == "--" {
        return true;
    }
    s.starts_with('-') && s.len() > 1 && !is_negative_number(s)
}

fn is_negative_number(s: &str) -> bool {
    let t = s.strip_prefix('-').unwrap_or(s);
    !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

fn is_valued(name: &str, valued: &[&str]) -> bool {
    let name = strip_dashes(name);
    valued.iter().any(|v| strip_dashes(v) == name)
}

fn take_value(
    raw: &[&'static str],
    i: &mut usize,
    name: &str,
    valued: &[&str],
    force: bool,
) -> Option<&'static str> {
    if *i >= raw.len() {
        return None;
    }
    if looks_like_flag(raw[*i]) {
        return None;
    }
    if force || is_valued(name, valued) {
        let v = raw[*i];
        *i += 1;
        Some(v)
    } else {
        None
    }
}

fn parse_slice(raw: &[&'static str], valued: &[&str]) -> Args {
    let mut program = "";
    let mut positionals = Vec::new();
    let mut options = Vec::new();
    let mut i = 0;

    if let Some(&prog) = raw.get(0) {
        program = prog;
        i = 1;
    }

    let mut end_opts = false;
    while i < raw.len() {
        let cur = raw[i];
        i += 1;

        if end_opts {
            positionals.push(cur);
            continue;
        }
        if cur == "--" {
            end_opts = true;
            continue;
        }

        if let Some(rest) = cur.strip_prefix("--") {
            if rest.is_empty() {
                positionals.push(cur);
                continue;
            }
            if let Some((name, val)) = rest.split_once('=') {
                options.push(Opt {
                    name,
                    value: Some(val),
                });
            } else {
                let force = valued.is_empty() || is_valued(rest, valued);
                let value = take_value(raw, &mut i, rest, valued, force);
                options.push(Opt { name: rest, value });
            }
            continue;
        }

        if let Some(rest) = cur.strip_prefix('-') {
            if rest.is_empty() {
                positionals.push(cur);
                continue;
            }
            if rest.starts_with(|c: char| c.is_ascii_digit()) {
                positionals.push(cur);
                continue;
            }
            if let Some((name, val)) = rest.split_once('=') {
                options.push(Opt {
                    name,
                    value: Some(val),
                });
            } else if rest.len() == 1 {
                let value = take_value(raw, &mut i, rest, valued, false);
                options.push(Opt { name: rest, value });
            } else {
                for (off, ch) in rest.char_indices() {
                    let name = &rest[off..off + ch.len_utf8()];
                    options.push(Opt { name, value: None });
                }
            }
            continue;
        }

        positionals.push(cur);
    }

    Args {
        program,
        positionals,
        options,
    }
}
