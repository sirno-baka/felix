//! VT screen for the shell. Lives here, not in libfelix — only the shell is a tty.
//!
//! Soft-wrap + word-wrap: overflow moves the last word to the next row and marks
//! the previous row as a continuation. Resize unwraps those rows and reflows.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::Rgb888,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use embedded_graphics_unicodefonts::mono_9x18_atlas;
use libfelix::wm::Window;
use vte::{Params, Parser, Perform};

pub const CELL_W: i32 = 9;
pub const CELL_H: i32 = 18;

const DEFAULT_FG: Rgb888 = Rgb888::new(0xF0, 0xF0, 0xF0);
const DEFAULT_BG: Rgb888 = Rgb888::new(0x10, 0x18, 0x20);
const SCROLLBACK_MAX: usize = 256;

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    fg: Rgb888,
    bg: Rgb888,
}

impl Cell {
    const fn empty() -> Self {
        Self {
            ch: ' ',
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
        }
    }
}

struct SbLine {
    cells: Vec<Cell>,
    soft: bool,
}

pub struct Terminal {
    parser: Parser,
    screen: Screen,
}

struct Screen {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    /// Per visible row: true = wrapped continuation, not a hard newline.
    row_soft: Vec<bool>,
    scrollback: Vec<SbLine>,
    view_off: usize,
    row: usize,
    col: usize,
    fg: Rgb888,
    bg: Rgb888,
    inverse: bool,
    dirty: bool,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            parser: Parser::new(),
            screen: Screen {
                cols,
                rows,
                cells: vec![Cell::empty(); cols * rows],
                row_soft: vec![false; rows],
                scrollback: Vec::new(),
                view_off: 0,
                row: 0,
                col: 0,
                fg: DEFAULT_FG,
                bg: DEFAULT_BG,
                inverse: false,
                dirty: true,
            },
        }
    }

    pub fn process(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.parser.advance(&mut self.screen, b);
        }
        self.screen.view_off = 0;
    }

    pub fn scroll(&mut self, delta: i32) {
        self.screen.scroll_view(delta);
    }

    pub fn write_str(&mut self, s: &str) {
        self.process(s.as_bytes());
    }

    pub fn clear(&mut self) {
        self.screen.clear_all();
        self.screen.row = 0;
        self.screen.col = 0;
    }

    pub fn visible_lines(&self) -> Vec<String> {
        (0..self.screen.rows)
            .map(|r| self.screen.row_string(r))
            .collect()
    }

    pub fn draw(&self, win: &mut Window, origin: Point) {
        self.screen.draw(win, origin);
    }

    pub fn cols(&self) -> usize {
        self.screen.cols
    }

    pub fn rows(&self) -> usize {
        self.screen.rows
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.screen.resize(cols, rows);
    }
}

impl Screen {
    fn idx(&self, r: usize, c: usize) -> usize {
        r * self.cols + c
    }

    fn last_content_row(&self) -> usize {
        let mut last = self.row;
        for r in 0..self.rows {
            let start = r * self.cols;
            if self.cells[start..start + self.cols]
                .iter()
                .any(|c| c.ch != ' ')
            {
                last = last.max(r);
            }
        }
        last
    }

    fn collect_physical(&self) -> Vec<(Vec<Cell>, bool)> {
        let mut phys = Vec::new();
        for line in &self.scrollback {
            let mut cells = line.cells.clone();
            cells.resize(self.cols, Cell::empty());
            let full = cells.last().map(|c| c.ch != ' ').unwrap_or(false);
            phys.push((cells, line.soft || full));
        }
        let last = self.last_content_row();
        for r in 0..=last {
            let start = r * self.cols;
            let cells = self.cells[start..start + self.cols].to_vec();
            let full = cells.last().map(|c| c.ch != ' ').unwrap_or(false);
            let soft = self.row_soft[r] || (full && r < last);
            phys.push((cells, soft));
        }
        phys
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }

        let phys = self.collect_physical();
        let mut logical: Vec<Vec<Cell>> = Vec::new();
        let mut acc: Vec<Cell> = Vec::new();
        for (cells, soft) in phys {
            acc.extend(trim_cells(&cells));
            if !soft {
                logical.push(core::mem::take(&mut acc));
            } else if !acc.is_empty() && acc.last().map(|c| c.ch != ' ').unwrap_or(false) {
                acc.push(Cell::empty());
            }
        }
        if !acc.is_empty() {
            logical.push(acc);
        }
        if logical.is_empty() {
            logical.push(Vec::new());
        }

        let mut wrapped: Vec<(Vec<Cell>, bool)> = Vec::new();
        for line in logical {
            wrapped.extend(wrap_words(&line, cols));
        }

        while wrapped.len() > SCROLLBACK_MAX + rows {
            wrapped.remove(0);
        }

        let screen_n = wrapped.len().min(rows);
        let split = wrapped.len() - screen_n;
        let sb: Vec<SbLine> = wrapped
            .drain(..split)
            .map(|(cells, soft)| SbLine { cells, soft })
            .collect();

        let mut new_cells = vec![Cell::empty(); cols * rows];
        let mut new_soft = vec![false; rows];
        let mut last_col = 0usize;
        let mut last_row = 0usize;
        for (r, (cells, soft)) in wrapped.into_iter().enumerate() {
            let n = cells.len().min(cols);
            new_cells[r * cols..r * cols + n].copy_from_slice(&cells[..n]);
            new_soft[r] = soft;
            last_row = r;
            last_col = trim_cells(&cells).len().min(cols);
        }

        self.scrollback = sb;
        self.cells = new_cells;
        self.row_soft = new_soft;
        self.cols = cols;
        self.rows = rows;
        self.row = last_row.min(rows - 1);
        self.col = last_col.min(cols);
        if self.col == cols {
            self.col = cols - 1;
        }
        self.view_off = 0;
        self.dirty = true;
    }

    fn current_pair(&self) -> (Rgb888, Rgb888) {
        if self.inverse {
            (self.bg, self.fg)
        } else {
            (self.fg, self.bg)
        }
    }

    fn put(&mut self, ch: char) {
        if self.col >= self.cols {
            self.wrap_current_word();
        }
        let (fg, bg) = self.current_pair();
        let i = self.idx(self.row, self.col);
        self.cells[i] = Cell { ch, fg, bg };
        self.col += 1;
        self.dirty = true;
    }

    fn wrap_current_word(&mut self) {
        let start = self.row * self.cols;
        let mut break_at = None;
        for c in (0..self.cols).rev() {
            if self.cells[start + c].ch == ' ' {
                break_at = Some(c);
                break;
            }
        }
        let overflow = match break_at {
            Some(sp) if sp + 1 < self.cols => {
                let ov = self.cells[start + sp + 1..start + self.cols].to_vec();
                for c in self.cells[start + sp + 1..start + self.cols].iter_mut() {
                    *c = Cell::empty();
                }
                ov
            }
            _ => Vec::new(),
        };
        self.row_soft[self.row] = true;
        self.col = 0;
        self.line_feed();
        for cell in overflow {
            if self.col >= self.cols {
                self.row_soft[self.row] = true;
                self.col = 0;
                self.line_feed();
            }
            let i = self.idx(self.row, self.col);
            self.cells[i] = cell;
            self.col += 1;
        }
    }

    fn hard_break(&mut self) {
        if self.row < self.row_soft.len() {
            self.row_soft[self.row] = false;
        }
        self.col = 0;
        self.line_feed();
    }

    fn line_feed(&mut self) {
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.scroll_up();
        }
        self.dirty = true;
    }

    fn scroll_up(&mut self) {
        let line: Vec<Cell> = self.cells.drain(0..self.cols).collect();
        let soft = if self.row_soft.is_empty() {
            false
        } else {
            self.row_soft.remove(0)
        };
        self.scrollback.push(SbLine { cells: line, soft });
        while self.scrollback.len() > SCROLLBACK_MAX {
            self.scrollback.remove(0);
        }
        self.cells
            .extend(core::iter::repeat(Cell::empty()).take(self.cols));
        self.row_soft.push(false);
        self.view_off = 0;
    }

    fn scroll_view(&mut self, delta: i32) {
        let max = self.scrollback.len();
        let next = (self.view_off as i32 + delta).clamp(0, max as i32);
        if next as usize != self.view_off {
            self.view_off = next as usize;
            self.dirty = true;
        }
    }

    fn view_start(&self) -> usize {
        let total = self.scrollback.len() + self.rows;
        total.saturating_sub(self.rows + self.view_off)
    }

    fn cells_for_abs(&self, abs: usize) -> Vec<Cell> {
        if abs < self.scrollback.len() {
            let mut row = self.scrollback[abs].cells.clone();
            row.resize(self.cols, Cell::empty());
            row
        } else {
            let r = abs - self.scrollback.len();
            if r >= self.rows {
                return vec![Cell::empty(); self.cols];
            }
            let start = r * self.cols;
            self.cells[start..start + self.cols].to_vec()
        }
    }

    fn clear_all(&mut self) {
        for c in &mut self.cells {
            *c = Cell::empty();
        }
        for s in &mut self.row_soft {
            *s = false;
        }
        self.dirty = true;
    }

    fn clear_range(&mut self, start: usize, end: usize) {
        let end = end.min(self.cells.len());
        for c in &mut self.cells[start..end] {
            *c = Cell::empty();
        }
        self.dirty = true;
    }

    fn row_string(&self, row: usize) -> String {
        if row >= self.rows {
            return String::new();
        }
        let start = row * self.cols;
        let mut s = String::new();
        for c in &self.cells[start..start + self.cols] {
            s.push(c.ch);
        }
        while s.ends_with(' ') {
            s.pop();
        }
        s
    }

    fn cursor_clamp(&mut self) {
        if self.row >= self.rows {
            self.row = self.rows - 1;
        }
        if self.col >= self.cols {
            self.col = self.cols - 1;
        }
    }

    fn draw(&self, win: &mut Window, origin: Point) {
        self.draw_rows(win, origin, self.rows, None);
        if self.view_off == 0 {
            let x = origin.x + (self.col as i32) * CELL_W;
            let y = origin.y + (self.row as i32) * CELL_H;
            let _ = Rectangle::new(Point::new(x, y + CELL_H - 2), Size::new(CELL_W as u32, 2))
                .into_styled(PrimitiveStyle::with_fill(DEFAULT_FG))
                .draw(win);
        }
    }

    fn draw_rows(&self, win: &mut Window, origin: Point, hist_rows: usize, input: Option<&str>) {
        let font = mono_9x18_atlas();
        let w = (self.cols as i32) * CELL_W;
        let h = (self.rows as i32) * CELL_H;
        let _ = Rectangle::new(origin, Size::new(w as u32, h as u32))
            .into_styled(PrimitiveStyle::with_fill(DEFAULT_BG))
            .draw(win);

        let start = self.view_start();
        for r in 0..hist_rows {
            self.draw_row(win, origin, r, start + r, &font);
        }
        if let Some(line) = input {
            let r = self.rows.saturating_sub(1);
            let y = origin.y + (r as i32) * CELL_H;
            let style = MonoTextStyle::new(&font, DEFAULT_FG);
            let mut buf = [0u8; 4];
            let mut x = origin.x;
            for ch in line.chars().chain(core::iter::once('_')) {
                let s = ch.encode_utf8(&mut buf);
                let _ = Text::with_baseline(s, Point::new(x, y), style, Baseline::Top).draw(win);
                x += CELL_W;
            }
        }
    }

    fn draw_row(
        &self,
        win: &mut Window,
        origin: Point,
        r: usize,
        abs: usize,
        font: &embedded_graphics::mono_font::MonoFont<'_>,
    ) {
        let y = origin.y + (r as i32) * CELL_H;
        let row = self.cells_for_abs(abs);
        let mut c = 0;
        while c < self.cols {
            let cell = row[c];
            let mut n = 1;
            while c + n < self.cols {
                let nxt = row[c + n];
                if nxt.fg != cell.fg || nxt.bg != cell.bg {
                    break;
                }
                n += 1;
            }
            let x = origin.x + (c as i32) * CELL_W;
            if cell.bg != DEFAULT_BG {
                let _ = Rectangle::new(
                    Point::new(x, y),
                    Size::new((n as u32) * CELL_W as u32, CELL_H as u32),
                )
                .into_styled(PrimitiveStyle::with_fill(cell.bg))
                .draw(win);
            }
            let style = MonoTextStyle::new(font, cell.fg);
            let mut text = String::new();
            for i in 0..n {
                text.push(row[c + i].ch);
            }
            if text.chars().any(|ch| ch != ' ') {
                let _ = Text::with_baseline(&text, Point::new(x, y), style, Baseline::Top).draw(win);
            }
            c += n;
        }
    }

    fn sgr(&mut self, params: &Params) {
        if params.iter().next().is_none() {
            self.fg = DEFAULT_FG;
            self.bg = DEFAULT_BG;
            self.inverse = false;
            return;
        }
        for p in params.iter() {
            let code = p.first().copied().unwrap_or(0);
            match code {
                0 => {
                    self.fg = DEFAULT_FG;
                    self.bg = DEFAULT_BG;
                    self.inverse = false;
                }
                7 => self.inverse = true,
                27 => self.inverse = false,
                30..=37 => self.fg = ansi_color(code - 30, false),
                39 => self.fg = DEFAULT_FG,
                40..=47 => self.bg = ansi_color(code - 40, false),
                49 => self.bg = DEFAULT_BG,
                90..=97 => self.fg = ansi_color(code - 90, true),
                100..=107 => self.bg = ansi_color(code - 100, true),
                _ => {}
            }
        }
    }
}

fn trim_cells(line: &[Cell]) -> Vec<Cell> {
    let mut end = line.len();
    while end > 0 && line[end - 1].ch == ' ' {
        end -= 1;
    }
    line[..end].to_vec()
}

fn pad_row(mut row: Vec<Cell>, width: usize) -> Vec<Cell> {
    row.resize(width, Cell::empty());
    row
}

fn wrap_words(src: &[Cell], width: usize) -> Vec<(Vec<Cell>, bool)> {
    let width = width.max(1);
    if src.is_empty() {
        return vec![(vec![Cell::empty(); width], false)];
    }
    let mut out: Vec<(Vec<Cell>, bool)> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let start = i;
        if src[i].ch == ' ' {
            while i < src.len() && src[i].ch == ' ' {
                i += 1;
            }
        } else {
            while i < src.len() && src[i].ch != ' ' {
                i += 1;
            }
        }
        let tok = &src[start..i];
        if tok.is_empty() {
            break;
        }
        let spaces = tok.iter().all(|c| c.ch == ' ');
        if spaces {
            if row.is_empty() {
                continue;
            }
            if row.len() + tok.len() <= width {
                row.extend_from_slice(tok);
            } else {
                out.push((pad_row(core::mem::take(&mut row), width), true));
            }
            continue;
        }
        if !row.is_empty() && row.len() + tok.len() > width {
            out.push((pad_row(core::mem::take(&mut row), width), true));
        }
        if tok.len() > width {
            let mut t = 0;
            while t < tok.len() {
                let room = width - row.len();
                let take = room.min(tok.len() - t);
                row.extend_from_slice(&tok[t..t + take]);
                t += take;
                if row.len() == width {
                    let more = t < tok.len() || i < src.len();
                    out.push((pad_row(core::mem::take(&mut row), width), more));
                }
            }
        } else {
            row.extend_from_slice(tok);
        }
    }
    out.push((pad_row(row, width), false));
    out
}

fn first_param(params: &Params, default: u16) -> u16 {
    params
        .iter()
        .next()
        .and_then(|s| s.first().copied())
        .filter(|&v| v != 0)
        .unwrap_or(default)
}

fn nth_raw(params: &Params, n: usize) -> u16 {
    params
        .iter()
        .nth(n)
        .and_then(|s| s.first().copied())
        .unwrap_or(0)
}

fn ansi_color(idx: u16, bright: bool) -> Rgb888 {
    let base: [(u8, u8, u8); 8] = [
        (0x00, 0x00, 0x00),
        (0xCC, 0x24, 0x1D),
        (0x98, 0x97, 0x1A),
        (0xD7, 0x99, 0x21),
        (0x45, 0x85, 0x88),
        (0xB1, 0x62, 0x86),
        (0x68, 0x9D, 0x6A),
        (0xA8, 0x99, 0x84),
    ];
    let (r, g, b) = base[idx.min(7) as usize];
    if bright {
        Rgb888::new(
            r.saturating_add(40),
            g.saturating_add(40),
            b.saturating_add(40),
        )
    } else {
        Rgb888::new(r, g, b)
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => {
                if self.col > 0 {
                    self.col -= 1;
                }
                self.dirty = true;
            }
            0x09 => {
                self.col = ((self.col / 8) + 1) * 8;
                if self.col >= self.cols {
                    self.row_soft[self.row] = true;
                    self.col = 0;
                    self.line_feed();
                }
                self.dirty = true;
            }
            0x0A | 0x0B | 0x0C => {
                self.hard_break();
            }
            0x0D => {
                self.col = 0;
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, _intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        match action {
            'A' => {
                let n = first_param(params, 1) as usize;
                self.row = self.row.saturating_sub(n);
            }
            'B' => {
                let n = first_param(params, 1) as usize;
                self.row = (self.row + n).min(self.rows - 1);
            }
            'C' => {
                let n = first_param(params, 1) as usize;
                self.col = (self.col + n).min(self.cols - 1);
            }
            'D' => {
                let n = first_param(params, 1) as usize;
                self.col = self.col.saturating_sub(n);
            }
            'H' | 'f' => {
                let row = first_param(params, 1) as usize;
                let col = nth_raw(params, 1);
                let col = if col == 0 { 1 } else { col } as usize;
                self.row = row.saturating_sub(1).min(self.rows - 1);
                self.col = col.saturating_sub(1).min(self.cols - 1);
            }
            'J' => {
                let n = nth_raw(params, 0);
                let cur = self.idx(self.row, self.col);
                match n {
                    0 => self.clear_range(cur, self.cells.len()),
                    1 => self.clear_range(0, cur.saturating_add(1)),
                    2 | 3 => self.clear_all(),
                    _ => {}
                }
            }
            'K' => {
                let n = nth_raw(params, 0);
                let line = self.row * self.cols;
                match n {
                    0 => self.clear_range(line + self.col, line + self.cols),
                    1 => self.clear_range(line, line + self.col + 1),
                    2 => self.clear_range(line, line + self.cols),
                    _ => {}
                }
            }
            'm' => self.sgr(params),
            _ => {}
        }
        self.cursor_clamp();
        self.dirty = true;
    }
}
