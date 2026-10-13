//! Symbols that compiled Rust code expects from a C runtime, whatever the
//! architecture: the floating-point remainders LLVM emits calls to. The
//! rest are the architecture's (`arch`).

#[unsafe(no_mangle)]
pub extern "C" fn fmodf(x: f32, y: f32) -> f32 {
    fmod(x as f64, y as f64) as f32
}

#[unsafe(no_mangle)]
pub extern "C" fn fmod(x: f64, y: f64) -> f64 {
    if y == 0.0 || x.is_nan() || y.is_nan() || x.is_infinite() {
        return f64::NAN;
    }
    if y.is_infinite() {
        return x;
    }
    // x - trunc(x / y) * y, computed exactly by repeated scaled subtraction.
    let (ax, ay) = (x.abs(), y.abs());
    if ax < ay {
        return x;
    }
    let mut r = ax;
    while r >= ay {
        let mut d = ay;
        while d * 2.0 <= r {
            d *= 2.0;
        }
        r -= d;
    }
    if x < 0.0 { -r } else { r }
}
