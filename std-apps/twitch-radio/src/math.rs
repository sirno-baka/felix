//! PopugOS does not have a system libm yet. Symphonia's AAC decoder expects
//! these C symbols because floating-point intrinsics lower to them on i686.

#[unsafe(no_mangle)]
pub extern "C" fn sinf(value: f32) -> f32 {
    libm::sinf(value)
}

#[unsafe(no_mangle)]
pub extern "C" fn powf(value: f32, power: f32) -> f32 {
    libm::powf(value, power)
}

#[unsafe(no_mangle)]
pub extern "C" fn exp2f(value: f32) -> f32 {
    libm::exp2f(value)
}

#[unsafe(no_mangle)]
pub extern "C" fn sin(value: f64) -> f64 {
    libm::sin(value)
}

#[unsafe(no_mangle)]
pub extern "C" fn cos(value: f64) -> f64 {
    libm::cos(value)
}
