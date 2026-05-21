//HELLO
//Simple program to test libfelix

#![no_std]
#![no_main]

use core::panic::PanicInfo;
use libfelix::*;

/// Алгоритм 1: Ряд Мадхавы-Лейбница
/// π/4 = 1 - 1/3 + 1/5 - 1/7 + ...
fn pi_madhava_leibniz(iters: u32) -> f32 {
    let mut sum: f32 = 0.0;
    for i in 0..iters {
        let term: f32 = 1.0 / (2.0 * i as f32 + 1.0);
        if i % 2 == 0 {
            sum += term;
        } else {
            sum -= term;
        }
    }
    sum * 4.0
}

/// Алгоритм 2: Ряд Нилаканты
/// π = 3 + 4/(2×3×4) - 4/(4×5×6) + 4/(6×7×8) - ...
fn pi_nilakantha(iters: u32) -> f32 {
    let mut sum: f32 = 3.0;
    for i in 1..=iters {
        let sign = if i % 2 == 1 { 1.0 } else { -1.0 };
        let n = i as f32;
        let term = 4.0 / (2.0 * n * (2.0 * n + 1.0) * (2.0 * n + 2.0));
        sum += sign * term;
    }
    sum
}

/// Алгоритм 3: Алгоритм Гаусса-Legendre
/// Использует арифметико-геометрическое среднее (АГС)
fn pi_gauss_legendre() -> f32 {
    let eps: f32 = 1e-8;
    let mut a: f32 = 1.0;
    let mut b: f32 = libm::sqrtf(0.5);           // ← исправлено
    let mut t: f32 = 0.25;
    let mut x: f32 = 1.0;

    loop {
        let a_next = (a + b) / 2.0;
        let b_next = libm::sqrtf(a * b);          // ← исправлено
        let t_next = t - x * (a - a_next) * (a - a_next);
        let x_next = 2.0 * x;

        if libm::fabsf(a_next - a) < eps {        // ← исправлено
            let num = (a_next + b_next) * (a_next + b_next);
            let den = 4.0 * t_next;
            return num / den;
        }

        a = a_next;
        b = b_next;
        t = t_next;
        x = x_next;
    }
}

#[no_mangle]
#[link_section = ".start"]
pub extern "C" fn _start() {
    let a = 0xFFFF;
    println!("Hello world! {:X}", a);

    let iters: u32 = 1_000_000;
    let pi1 = pi_madhava_leibniz(iters);
    println!("pi_madhava_leibniz {:?}it. {:.8}", iters, pi1);

    let pi2 = pi_nilakantha(iters);
    println!("pi_nilakantha {:?}it.: {:.8}", iters, pi2);

    let pi3 = pi_gauss_legendre();
    println!("pi_gauss_legendre: {:.8}", pi3);

}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
