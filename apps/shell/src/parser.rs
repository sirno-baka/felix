//! Small shell grammar for Felix.
//!
//! Supports words with single/double quotes and backslash escaping, pipelines,
//! command lists (`;`, `&&`, `||`, `&`) and fd redirections (`<`, `>`, `>>`,
//! `2>`, `2>>`, `2>&1`). It deliberately stays allocation-only / no_std.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Marker inserted before a character whose shell expansion must be suppressed
/// (single quotes or backslash). It is removed by the shell's word expander.
pub const PROTECTED: char = '\u{1f}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirKind {
    In,
    Out,
    Append,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RedirTarget {
    Path(String),
    Fd(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redir {
    pub fd: u8,
    pub kind: RedirKind,
    pub target: RedirTarget,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimpleCmd {
    pub args: Vec<String>,
    pub redirs: Vec<Redir>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connector {
    Always,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandGroup {
    pub pipeline: Vec<SimpleCmd>,
    pub background: bool,
    /// Relation from this group to the next one.
    pub next: Connector,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Pipe,
    Semi,
    AndIf,
    OrIf,
    Background,
    Redir { fd: u8, kind: RedirKind },
    Dup { fd: u8, target: u8 },
}

fn push_protected(dst: &mut String, ch: char) {
    if ch == '$' || ch == '~' || ch == PROTECTED {
        dst.push(PROTECTED);
    }
    dst.push(ch);
}

fn flush_word(cur: &mut String, out: &mut Vec<Token>, started: &mut bool) {
    if *started {
        out.push(Token::Word(core::mem::take(cur)));
        *started = false;
    }
}

fn take_fd_prefix(
    cur: &mut String,
    default: u8,
    out: &mut Vec<Token>,
    started: &mut bool,
) -> Result<u8, String> {
    if *started && !cur.is_empty() && cur.bytes().all(|b| b.is_ascii_digit()) {
        let fd = cur.parse::<u8>().map_err(|_| String::from("bad redirection fd"))?;
        cur.clear();
        *started = false;
        Ok(fd)
    } else {
        flush_word(cur, out, started);
        Ok(default)
    }
}

fn tokenize(line: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = line.chars().peekable();
    let mut single = false;
    let mut double = false;
    let mut word_started = false;

    while let Some(ch) = it.next() {
        if single {
            if ch == '\'' {
                single = false;
            } else {
                push_protected(&mut cur, ch);
            }
            continue;
        }
        if double {
            match ch {
                '"' => double = false,
                '\\' => {
                    let Some(next) = it.next() else {
                        return Err(String::from("unfinished escape in double quotes"));
                    };
                    push_protected(&mut cur, next);
                }
                _ => { word_started = true; cur.push(ch); }
            }
            continue;
        }

        match ch {
            '\'' => { single = true; word_started = true; }
            '"' => { double = true; word_started = true; }
            '\\' => {
                let Some(next) = it.next() else {
                    return Err(String::from("unfinished escape"));
                };
                word_started = true;
                push_protected(&mut cur, next);
            }
            c if c.is_whitespace() => flush_word(&mut cur, &mut out, &mut word_started),
            ';' => {
                flush_word(&mut cur, &mut out, &mut word_started);
                out.push(Token::Semi);
            }
            '|' => {
                flush_word(&mut cur, &mut out, &mut word_started);
                if it.peek() == Some(&'|') {
                    it.next();
                    out.push(Token::OrIf);
                } else {
                    out.push(Token::Pipe);
                }
            }
            '&' => {
                flush_word(&mut cur, &mut out, &mut word_started);
                if it.peek() == Some(&'&') {
                    it.next();
                    out.push(Token::AndIf);
                } else {
                    out.push(Token::Background);
                }
            }
            '<' | '>' => {
                let default_fd = if ch == '<' { 0 } else { 1 };
                let fd = take_fd_prefix(&mut cur, default_fd, &mut out, &mut word_started)?;
                let mut kind = if ch == '<' { RedirKind::In } else { RedirKind::Out };
                if ch == '>' && it.peek() == Some(&'>') {
                    it.next();
                    kind = RedirKind::Append;
                }

                // n>&m (the common fd-duplication form).
                if ch == '>' && kind == RedirKind::Out && it.peek() == Some(&'&') {
                    it.next();
                    let mut digits = String::new();
                    while let Some(c) = it.peek().copied() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        digits.push(c);
                        it.next();
                    }
                    if digits.is_empty() {
                        return Err(String::from("redirection: expected fd after >&"));
                    }
                    let target = digits
                        .parse::<u8>()
                        .map_err(|_| String::from("redirection: bad target fd"))?;
                    out.push(Token::Dup { fd, target });
                } else {
                    out.push(Token::Redir { fd, kind });
                }
            }
            _ => { word_started = true; cur.push(ch); }
        }
    }

    if single {
        return Err(String::from("unterminated single quote"));
    }
    if double {
        return Err(String::from("unterminated double quote"));
    }
    flush_word(&mut cur, &mut out, &mut word_started);
    Ok(out)
}

fn finish_cmd(cmd: &mut SimpleCmd, pipeline: &mut Vec<SimpleCmd>) -> Result<(), String> {
    if cmd.args.is_empty() && cmd.redirs.is_empty() {
        return Err(String::from("empty command"));
    }
    if cmd.args.is_empty() {
        return Err(String::from("redirection without command"));
    }
    pipeline.push(core::mem::take(cmd));
    Ok(())
}

fn finish_group(
    cmd: &mut SimpleCmd,
    pipeline: &mut Vec<SimpleCmd>,
    groups: &mut Vec<CommandGroup>,
    background: bool,
    next: Connector,
) -> Result<(), String> {
    finish_cmd(cmd, pipeline)?;
    groups.push(CommandGroup {
        pipeline: core::mem::take(pipeline),
        background,
        next,
    });
    Ok(())
}

pub fn parse_line(line: &str) -> Result<Vec<CommandGroup>, String> {
    let tokens = tokenize(line)?;
    if tokens.is_empty() {
        return Ok(Vec::new());
    }

    let mut groups = Vec::new();
    let mut pipeline = Vec::new();
    let mut cmd = SimpleCmd::default();
    let mut i = 0usize;

    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(w) => cmd.args.push(w.clone()),
            Token::Redir { fd, kind } => {
                i += 1;
                let Some(Token::Word(path)) = tokens.get(i) else {
                    return Err(String::from("redirection: expected path"));
                };
                cmd.redirs.push(Redir {
                    fd: *fd,
                    kind: *kind,
                    target: RedirTarget::Path(path.clone()),
                });
            }
            Token::Dup { fd, target } => cmd.redirs.push(Redir {
                fd: *fd,
                kind: RedirKind::Out,
                target: RedirTarget::Fd(*target),
            }),
            Token::Pipe => {
                finish_cmd(&mut cmd, &mut pipeline)?;
            }
            Token::Semi => finish_group(
                &mut cmd,
                &mut pipeline,
                &mut groups,
                false,
                Connector::Always,
            )?,
            Token::AndIf => finish_group(
                &mut cmd,
                &mut pipeline,
                &mut groups,
                false,
                Connector::And,
            )?,
            Token::OrIf => finish_group(
                &mut cmd,
                &mut pipeline,
                &mut groups,
                false,
                Connector::Or,
            )?,
            Token::Background => finish_group(
                &mut cmd,
                &mut pipeline,
                &mut groups,
                true,
                Connector::Always,
            )?,
        }
        i += 1;
    }

    if !cmd.args.is_empty() || !cmd.redirs.is_empty() || !pipeline.is_empty() {
        finish_group(
            &mut cmd,
            &mut pipeline,
            &mut groups,
            false,
            Connector::Always,
        )?;
    } else if matches!(tokens.last(), Some(Token::Pipe | Token::AndIf | Token::OrIf)) {
        return Err(String::from("operator requires a command"));
    }

    // A trailing ';' or '&' is valid. A trailing &&/|| is not.
    if matches!(tokens.last(), Some(Token::AndIf | Token::OrIf | Token::Pipe)) {
        return Err(format!("operator requires a command"));
    }

    Ok(groups)
}
