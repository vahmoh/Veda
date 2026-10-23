// SPDX-FileCopyrightText: © 2026 Vahid Mohammadi
// SPDX-License-Identifier: GPL-3.0-only

//! Scalar operations and their exact semantics.
//!
//! Every computation a shader does is lowered to these operations on
//! scalars ([`Value`]). [`bits`] defines what each one computes, bit for
//! bit, and is the single source of truth: [`eval`] applies it to fold
//! constant expressions at compile time (and the optimiser to fold what
//! became constant), and the interpreter applies the same functions to its
//! lanes. Where GLSL ES leaves a result undefined, the choice made here is
//! the one SSE hardware makes, so that the interpreter can use the plain
//! instruction:
//!
//! * `min`/`max` return the second operand when either is NaN (`minps`).
//! * Converting a float that is NaN or out of range to `int` gives
//!   `0x80000000` (`cvttps2dq`); to `uint` it saturates (NaN gives 0).
//! * Integer division by zero gives all ones (as Direct3D defines it),
//!   and so does the remainder; `INT_MIN / -1` wraps to `INT_MIN`.
//! * Shift amounts are taken modulo 32.
//! * `round` rounds halfway cases to even, like `roundEven`.
//!
//! Transcendental functions come from `libm`, so a constant folded on the
//! host or in Veda is the same value the interpreter computes at run time.

/// A scalar type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Ty {
    F32,
    I32,
    U32,
    Bool,
}

/// A scalar value. Integers of either signedness share the bit pattern of
/// their 32-bit representation; the operation says how to read it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Value {
    F(f32),
    I(i32),
    U(u32),
    B(bool),
}

impl Value {
    pub fn ty(self) -> Ty {
        match self {
            Value::F(_) => Ty::F32,
            Value::I(_) => Ty::I32,
            Value::U(_) => Ty::U32,
            Value::B(_) => Ty::Bool,
        }
    }

    /// The value's 32 bits, as stored in a register (`true` is all ones).
    pub fn bits(self) -> u32 {
        match self {
            Value::F(f) => f.to_bits(),
            Value::I(i) => i as u32,
            Value::U(u) => u,
            Value::B(b) => {
                if b {
                    !0
                } else {
                    0
                }
            }
        }
    }

    /// Reads 32 bits as a value of type `ty`.
    pub fn from_bits(ty: Ty, bits: u32) -> Value {
        match ty {
            Ty::F32 => Value::F(f32::from_bits(bits)),
            Ty::I32 => Value::I(bits as i32),
            Ty::U32 => Value::U(bits),
            Ty::Bool => Value::B(bits != 0),
        }
    }

    /// Bitwise identity: the same type and bits (so NaNs compare equal,
    /// and `0.0` differs from `-0.0`).
    pub fn same(self, other: Value) -> bool {
        self.ty() == other.ty() && self.bits() == other.bits()
    }
}

/// A scalar operation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Op {
    // float -> float
    FNeg,
    FAbs,
    FSign,
    FFloor,
    FCeil,
    FTrunc,
    FRoundEven,
    FFract,
    FSqrt,
    FRsq,
    FExp,
    FLog,
    FExp2,
    FLog2,
    FSin,
    FCos,
    FTan,
    FAsin,
    FAcos,
    FAtan,
    FSinh,
    FCosh,
    FTanh,
    FAsinh,
    FAcosh,
    FAtanh,
    // (float, float) -> float
    FAdd,
    FSub,
    FMul,
    FDiv,
    FMin,
    FMax,
    FPow,
    FAtan2,
    // (float, float) -> bool
    FLt,
    FLe,
    FEq,
    FNe,
    // float -> bool
    FIsNan,
    FIsInf,
    // int (either signedness) -> int
    INeg,
    INot,
    IAbs,
    ISign,
    // (int, int) -> int; the same bits whatever the signedness
    IAdd,
    ISub,
    IMul,
    IAnd,
    IOr,
    IXor,
    IShl,
    // signed and unsigned versions
    IDiv,
    UDiv,
    IRem,
    URem,
    IShr,
    UShr,
    IMin,
    UMin,
    IMax,
    UMax,
    // (int, int) -> bool
    IEq,
    INe,
    ILt,
    ULt,
    ILe,
    ULe,
    // bool
    BNot,
    BAnd,
    BOr,
    BXor,
    BEq,
    // conversions
    FToI,
    FToU,
    IToF,
    UToF,
    BToF,
    BToI,
    FToB,
    IToB,
    /// Reinterpret 32 bits as another type (`floatBitsToInt`...); also
    /// `int`/`uint` conversions, which keep the bits.
    Bitcast,
    /// Float to half precision: the half's bits in the low 16 bits.
    FToHalf,
    /// Half precision bits (the low 16) to float.
    HalfToF,
}

/// What an operation takes and gives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Signature {
    pub args: &'static [Ty],
    pub ret: Ty,
}

impl Op {
    /// The operation's operand and result types. `I32` stands for either
    /// integer type where the operation does not care (callers check).
    pub fn signature(self) -> Signature {
        use Op::*;
        use Ty::*;
        const F1: &[Ty] = &[F32];
        const F2: &[Ty] = &[F32, F32];
        const I1: &[Ty] = &[I32];
        const I2: &[Ty] = &[I32, I32];
        const B1: &[Ty] = &[Bool];
        const B2: &[Ty] = &[Bool, Bool];
        const U1: &[Ty] = &[U32];
        let (args, ret) = match self {
            FNeg | FAbs | FSign | FFloor | FCeil | FTrunc | FRoundEven | FFract | FSqrt | FRsq | FExp | FLog
            | FExp2 | FLog2 | FSin | FCos | FTan | FAsin | FAcos | FAtan | FSinh | FCosh | FTanh | FAsinh | FAcosh
            | FAtanh => (F1, F32),
            FAdd | FSub | FMul | FDiv | FMin | FMax | FPow | FAtan2 => (F2, F32),
            FLt | FLe | FEq | FNe => (F2, Bool),
            FIsNan | FIsInf => (F1, Bool),
            INeg | INot | IAbs | ISign => (I1, I32),
            IAdd | ISub | IMul | IAnd | IOr | IXor | IShl | IDiv | UDiv | IRem | URem | IShr | UShr | IMin | UMin
            | IMax | UMax => (I2, I32),
            IEq | INe | ILt | ULt | ILe | ULe => (I2, Bool),
            BNot => (B1, Bool),
            BAnd | BOr | BXor | BEq => (B2, Bool),
            FToI => (F1, I32),
            FToU => (F1, U32),
            IToF => (I1, F32),
            UToF => (U1, F32),
            BToF => (B1, F32),
            BToI => (B1, I32),
            FToB => (F1, Bool),
            IToB => (I1, Bool),
            Bitcast => (I1, I32),
            FToHalf => (F1, U32),
            HalfToF => (U1, F32),
        };
        Signature { args, ret }
    }

    /// The type of the result, given `ret` for the operations that leave
    /// it to the caller (the integer operations, of either signedness, and
    /// `Bitcast`).
    pub fn result(self, ret: Ty) -> Ty {
        match self.signature().ret {
            Ty::I32 => ret,
            t => t,
        }
    }

    /// Whether `op(a, b) == op(b, a)`.
    pub fn commutative(self) -> bool {
        use Op::*;
        matches!(
            self,
            FAdd | FMul
                | FMin
                | FMax
                | FEq
                | FNe
                | IAdd
                | IMul
                | IAnd
                | IOr
                | IXor
                | IMin
                | UMin
                | IMax
                | UMax
                | IEq
                | INe
                | BAnd
                | BOr
                | BXor
                | BEq
        )
    }
}

/// Computes `op` on `args`. `ret` gives the result type where the
/// operation does not fix it (the integer operations and `Bitcast`).
pub fn eval(op: Op, args: &[Value], ret: Ty) -> Value {
    let a = args.first().map_or(0, |v| v.bits());
    let b = args.get(1).map_or(0, |v| v.bits());
    Value::from_bits(op.result(ret), apply(op, ret, a, b))
}

/// `op` on one scalar's bits (`b` is ignored by operations of one
/// operand); `ret` as for [`eval`].
pub fn apply(op: Op, ret: Ty, a: u32, b: u32) -> u32 {
    use Op::*;
    use bits::*;
    match op {
        FNeg => f_neg(a),
        FAbs => f_abs(a),
        FSign => f_sign(a),
        FFloor => f_floor(a),
        FCeil => f_ceil(a),
        FTrunc => f_trunc(a),
        FRoundEven => f_round_even(a),
        FFract => f_fract(a),
        FSqrt => f_sqrt(a),
        FRsq => f_rsq(a),
        FExp => f_exp(a),
        FLog => f_log(a),
        FExp2 => f_exp2(a),
        FLog2 => f_log2(a),
        FSin => f_sin(a),
        FCos => f_cos(a),
        FTan => f_tan(a),
        FAsin => f_asin(a),
        FAcos => f_acos(a),
        FAtan => f_atan(a),
        FSinh => f_sinh(a),
        FCosh => f_cosh(a),
        FTanh => f_tanh(a),
        FAsinh => f_asinh(a),
        FAcosh => f_acosh(a),
        FAtanh => f_atanh(a),
        FAdd => f_add(a, b),
        FSub => f_sub(a, b),
        FMul => f_mul(a, b),
        FDiv => f_div(a, b),
        FMin => f_min(a, b),
        FMax => f_max(a, b),
        FPow => f_pow(a, b),
        FAtan2 => f_atan2(a, b),
        FLt => f_lt(a, b),
        FLe => f_le(a, b),
        FEq => f_eq(a, b),
        FNe => f_ne(a, b),
        FIsNan => f_is_nan(a),
        FIsInf => f_is_inf(a),
        INeg => i_neg(a),
        INot => i_not(a),
        IAbs => i_abs(a),
        ISign => i_sign(a),
        IAdd => i_add(a, b),
        ISub => i_sub(a, b),
        IMul => i_mul(a, b),
        IAnd => i_and(a, b),
        IOr => i_or(a, b),
        IXor => i_xor(a, b),
        IShl => i_shl(a, b),
        IDiv => i_div(a, b),
        UDiv => u_div(a, b),
        IRem => i_rem(a, b),
        URem => u_rem(a, b),
        IShr => i_shr(a, b),
        UShr => u_shr(a, b),
        IMin => i_min(a, b),
        UMin => u_min(a, b),
        IMax => i_max(a, b),
        UMax => u_max(a, b),
        IEq => i_eq(a, b),
        INe => i_ne(a, b),
        ILt => i_lt(a, b),
        ULt => u_lt(a, b),
        ILe => i_le(a, b),
        ULe => u_le(a, b),
        BNot => b_not(a),
        BAnd => b_and(a, b),
        BOr => b_or(a, b),
        BXor => b_xor(a, b),
        BEq => b_eq(a, b),
        FToI => f_to_i_bits(a),
        FToU => f_to_u_bits(a),
        IToF => i_to_f(a),
        UToF => u_to_f(a),
        BToF => b_to_f(a),
        BToI => b_to_i(a),
        FToB => f_to_b(a),
        IToB => i_to_b(a),
        Bitcast => bitcast(ret, a),
        FToHalf => f_to_half(a),
        HalfToF => half_to_f(a),
    }
}

/// What each operation computes, on 32-bit scalars as registers hold them
/// (`true` is all ones): the one definition [`eval`] and the interpreter's
/// lanes both apply. Each is small and inlined where it is applied.
pub mod bits {
    use super::Ty;

    #[inline(always)]
    fn f(x: u32) -> f32 {
        f32::from_bits(x)
    }

    #[inline(always)]
    fn bool(x: bool) -> u32 {
        0u32.wrapping_sub(u32::from(x))
    }

    /// Operations on one float: `name(x) = expression;`.
    macro_rules! float1 {
        ($($name:ident($x:ident) = $e:expr;)*) => {$(
            #[inline(always)]
            pub fn $name(a: u32) -> u32 {
                let $x = f(a);
                let r: f32 = $e;
                r.to_bits()
            }
        )*};
    }

    /// Operations on two floats: `name(x, y) = expression;`.
    macro_rules! float2 {
        ($($name:ident($x:ident, $y:ident) = $e:expr;)*) => {$(
            #[inline(always)]
            pub fn $name(a: u32, b: u32) -> u32 {
                let ($x, $y) = (f(a), f(b));
                let r: f32 = $e;
                r.to_bits()
            }
        )*};
    }

    /// Operations on two 32-bit words: `name(x, y) = expression;`.
    macro_rules! int2 {
        ($($name:ident($x:ident, $y:ident) = $e:expr;)*) => {$(
            #[inline(always)]
            pub fn $name($x: u32, $y: u32) -> u32 {
                $e
            }
        )*};
    }

    #[inline(always)]
    pub fn f_neg(a: u32) -> u32 {
        a ^ 0x8000_0000
    }

    #[inline(always)]
    pub fn f_abs(a: u32) -> u32 {
        a & 0x7FFF_FFFF
    }

    float1! {
        // 0, -0 and NaN keep themselves (NaN: undefined).
        f_sign(x) = if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { x };
        f_floor(x) = libm::floorf(x);
        f_ceil(x) = libm::ceilf(x);
        f_trunc(x) = libm::truncf(x);
        f_round_even(x) = libm::roundevenf(x);
        f_fract(x) = x - libm::floorf(x);
        f_sqrt(x) = libm::sqrtf(x);
        f_rsq(x) = 1.0 / libm::sqrtf(x);
        f_exp(x) = libm::expf(x);
        f_log(x) = libm::logf(x);
        f_exp2(x) = libm::exp2f(x);
        f_log2(x) = libm::log2f(x);
        f_sin(x) = libm::sinf(x);
        f_cos(x) = libm::cosf(x);
        f_tan(x) = libm::tanf(x);
        f_asin(x) = libm::asinf(x);
        f_acos(x) = libm::acosf(x);
        f_atan(x) = libm::atanf(x);
        f_sinh(x) = libm::sinhf(x);
        f_cosh(x) = libm::coshf(x);
        f_tanh(x) = libm::tanhf(x);
        f_asinh(x) = libm::asinhf(x);
        f_acosh(x) = libm::acoshf(x);
        f_atanh(x) = libm::atanhf(x);
    }

    float2! {
        f_add(x, y) = x + y;
        f_sub(x, y) = x - y;
        f_mul(x, y) = x * y;
        f_div(x, y) = x / y;
        // minps/maxps: the second operand unless the comparison holds.
        f_min(x, y) = if x < y { x } else { y };
        f_max(x, y) = if x > y { x } else { y };
        f_pow(x, y) = libm::powf(x, y);
        f_atan2(x, y) = libm::atan2f(x, y);
    }

    #[inline(always)]
    pub fn f_lt(a: u32, b: u32) -> u32 {
        bool(f(a) < f(b))
    }

    #[inline(always)]
    pub fn f_le(a: u32, b: u32) -> u32 {
        bool(f(a) <= f(b))
    }

    #[inline(always)]
    pub fn f_eq(a: u32, b: u32) -> u32 {
        bool(f(a) == f(b))
    }

    #[inline(always)]
    pub fn f_ne(a: u32, b: u32) -> u32 {
        bool(f(a) != f(b))
    }

    #[inline(always)]
    pub fn f_is_nan(a: u32) -> u32 {
        bool(f(a).is_nan())
    }

    #[inline(always)]
    pub fn f_is_inf(a: u32) -> u32 {
        bool(f(a).is_infinite())
    }

    #[inline(always)]
    pub fn i_neg(a: u32) -> u32 {
        (a as i32).wrapping_neg() as u32
    }

    #[inline(always)]
    pub fn i_not(a: u32) -> u32 {
        !a
    }

    #[inline(always)]
    pub fn i_abs(a: u32) -> u32 {
        (a as i32).wrapping_abs() as u32
    }

    #[inline(always)]
    pub fn i_sign(a: u32) -> u32 {
        (a as i32).signum() as u32
    }

    int2! {
        i_add(x, y) = x.wrapping_add(y);
        i_sub(x, y) = x.wrapping_sub(y);
        i_mul(x, y) = x.wrapping_mul(y);
        i_and(x, y) = x & y;
        i_or(x, y) = x | y;
        i_xor(x, y) = x ^ y;
        // Shift amounts are taken modulo 32.
        i_shl(x, y) = x.wrapping_shl(y & 31);
        i_shr(x, y) = ((x as i32) >> (y & 31)) as u32;
        u_shr(x, y) = x >> (y & 31);
        // Division by zero gives all ones; INT_MIN / -1 wraps.
        i_div(x, y) = if y == 0 { !0 } else { (x as i32).wrapping_div(y as i32) as u32 };
        u_div(x, y) = x.checked_div(y).unwrap_or(!0);
        i_rem(x, y) = if y == 0 { !0 } else { (x as i32).wrapping_rem(y as i32) as u32 };
        u_rem(x, y) = x.checked_rem(y).unwrap_or(!0);
        i_min(x, y) = (x as i32).min(y as i32) as u32;
        u_min(x, y) = x.min(y);
        i_max(x, y) = (x as i32).max(y as i32) as u32;
        u_max(x, y) = x.max(y);
        i_eq(x, y) = bool(x == y);
        i_ne(x, y) = bool(x != y);
        i_lt(x, y) = bool((x as i32) < (y as i32));
        u_lt(x, y) = bool(x < y);
        i_le(x, y) = bool((x as i32) <= (y as i32));
        u_le(x, y) = bool(x <= y);
        b_and(x, y) = bool(x != 0 && y != 0);
        b_or(x, y) = bool(x != 0 || y != 0);
        b_xor(x, y) = bool((x != 0) != (y != 0));
        b_eq(x, y) = bool((x != 0) == (y != 0));
    }

    #[inline(always)]
    pub fn b_not(a: u32) -> u32 {
        bool(a == 0)
    }

    #[inline(always)]
    pub fn f_to_i_bits(a: u32) -> u32 {
        super::f_to_i(f(a)) as u32
    }

    #[inline(always)]
    pub fn f_to_u_bits(a: u32) -> u32 {
        super::f_to_u(f(a))
    }

    #[inline(always)]
    pub fn i_to_f(a: u32) -> u32 {
        (a as i32 as f32).to_bits()
    }

    #[inline(always)]
    pub fn u_to_f(a: u32) -> u32 {
        (a as f32).to_bits()
    }

    #[inline(always)]
    pub fn b_to_f(a: u32) -> u32 {
        if a != 0 { 1.0f32.to_bits() } else { 0 }
    }

    #[inline(always)]
    pub fn b_to_i(a: u32) -> u32 {
        u32::from(a != 0)
    }

    #[inline(always)]
    pub fn f_to_b(a: u32) -> u32 {
        bool(f(a) != 0.0)
    }

    #[inline(always)]
    pub fn i_to_b(a: u32) -> u32 {
        bool(a != 0)
    }

    /// The same bits, but a bool result normalised (anything non-zero is
    /// true).
    #[inline(always)]
    pub fn bitcast(ret: Ty, a: u32) -> u32 {
        if ret == Ty::Bool { bool(a != 0) } else { a }
    }

    #[inline(always)]
    pub fn f_to_half(a: u32) -> u32 {
        u32::from(super::f32_to_f16(f(a)))
    }

    #[inline(always)]
    pub fn half_to_f(a: u32) -> u32 {
        super::f16_to_f32(a as u16).to_bits()
    }
}

/// `cvttps2dq`: truncation, or `0x80000000` for NaN and out-of-range values.
pub fn f_to_i(x: f32) -> i32 {
    if x.is_nan() || !(-2147483648.0..2147483648.0).contains(&x) { i32::MIN } else { x as i32 }
}

/// Truncation to `uint`, saturating (NaN gives 0).
pub fn f_to_u(x: f32) -> u32 {
    x as u32
}

/// Converts to half precision, rounding to nearest even; overflow gives
/// infinity, NaN stays NaN.
pub fn f32_to_f16(x: f32) -> u16 {
    let bits = x.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let mant = bits & 0x7F_FFFF;
    if exp == 0xFF {
        // Infinity or NaN (keep a quiet NaN).
        return sign | 0x7C00 | if mant != 0 { 0x200 | (mant >> 13) as u16 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1F {
        return sign | 0x7C00;
    }
    if e <= 0 {
        // A subnormal half, or zero.
        if e < -10 {
            return sign;
        }
        let m = mant | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rem = m & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        let rounded = if rem > halfway || (rem == halfway && half & 1 == 1) { half + 1 } else { half };
        return sign | rounded as u16;
    }
    let half = ((e as u32) << 10) | (mant >> 13);
    let rem = mant & 0x1FFF;
    let rounded = if rem > 0x1000 || (rem == 0x1000 && half & 1 == 1) { half + 1 } else { half };
    // A carry into the exponent is correct, up to infinity.
    sign | rounded as u16
}

/// Converts half precision bits to a float, exactly.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h & 0x8000) << 16;
    let exp = u32::from((h >> 10) & 0x1F);
    let mant = u32::from(h & 0x3FF);
    let bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            // Normalise the subnormal.
            let mut e = 113u32;
            let mut m = mant;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x3FF) << 13)
        }
    } else if exp == 0x1F {
        sign | 0x7F80_0000 | (mant << 13)
    } else {
        sign | ((exp + 112) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}
