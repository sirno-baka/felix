//! Module containing little utils

use crate::time::jiffies;

pub mod arcm;
pub mod flags;
pub mod queue;


pub fn rand_int(a: i32, b: i32) -> i32 {
    let mut seed = jiffies() as u64;
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;

    let (min, max) = if a <= b { (a as i64, b as i64) } else { (b as i64, a as i64) };
    let range = (max - min + 1) as u64;

    (min + (seed % range) as i64) as i32
}