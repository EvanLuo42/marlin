//! Operations without side effects: arithmetic, comparisons, conversions, math, vectors,
//! matrices and aggregates (0006 §8.1–§8.3, §10).

use crate::interp::{ExecError, Val};
use crate::math;
use crate::module::Module;
use crate::num::{f16_to_f32, f64_to_f16};
use crate::op::{BinaryOp, ConvertOp, FloatCmp, IntCmp, MathOp, Op, UnaryOp};
use crate::ty::{Scalar, Type, TypeData};

type R<T> = Result<T, ExecError>;

fn shape_error() -> ExecError {
    ExecError::new("an operand does not have the shape of its type")
}

pub(crate) fn mask(s: Scalar) -> u64 {
    if s.bits() >= 64 {
        u64::MAX
    } else {
        (1 << s.bits()) - 1
    }
}

/// The bits read as a signed integer of the scalar's width.
pub(crate) fn sext(b: u64, s: Scalar) -> i64 {
    let shift = 64 - s.bits();
    ((b << shift) as i64) >> shift
}

pub(crate) fn to_f64(b: u64, s: Scalar) -> f64 {
    match s {
        Scalar::F16 => f16_to_f32(b as u16) as f64,
        Scalar::F32 => f32::from_bits(b as u32) as f64,
        _ => f64::from_bits(b),
    }
}

/// `x` rounded to the float type `s`, as bits.
pub(crate) fn from_f64(x: f64, s: Scalar) -> u64 {
    match s {
        Scalar::F16 => f64_to_f16(x) as u64,
        Scalar::F32 => (x as f32).to_bits() as u64,
        _ => x.to_bits(),
    }
}

fn bits(v: &Val) -> R<u64> {
    v.bits().ok_or_else(shape_error)
}

fn parts(v: &Val) -> R<&[Val]> {
    v.parts().ok_or_else(shape_error)
}

/// Applies `f` to each scalar of `v` (a scalar, vector or matrix).
fn map1(v: &Val, f: &mut impl FnMut(u64) -> u64) -> R<Val> {
    Ok(match v {
        Val::Bits(b) => Val::Bits(f(*b)),
        Val::Agg(items) => Val::Agg(items.iter().map(|i| map1(i, f)).collect::<R<_>>()?),
        _ => return Err(shape_error()),
    })
}

fn map2(a: &Val, b: &Val, f: &mut impl FnMut(u64, u64) -> u64) -> R<Val> {
    Ok(match (a, b) {
        (Val::Bits(x), Val::Bits(y)) => Val::Bits(f(*x, *y)),
        (Val::Agg(xs), Val::Agg(ys)) if xs.len() == ys.len() => Val::Agg(
            xs.iter()
                .zip(ys)
                .map(|(x, y)| map2(x, y, f))
                .collect::<R<_>>()?,
        ),
        _ => return Err(shape_error()),
    })
}

fn map3(a: &Val, b: &Val, c: &Val, f: &mut impl FnMut(u64, u64, u64) -> u64) -> R<Val> {
    Ok(match (a, b, c) {
        (Val::Bits(x), Val::Bits(y), Val::Bits(z)) => Val::Bits(f(*x, *y, *z)),
        (Val::Agg(xs), Val::Agg(ys), Val::Agg(zs))
            if xs.len() == ys.len() && ys.len() == zs.len() =>
        {
            Val::Agg(
                xs.iter()
                    .zip(ys)
                    .zip(zs)
                    .map(|((x, y), z)| map3(x, y, z, f))
                    .collect::<R<_>>()?,
            )
        }
        _ => return Err(shape_error()),
    })
}

fn scalar_of(m: &Module, t: Type) -> R<Scalar> {
    m.element_scalar(t)
        .ok_or_else(|| ExecError::new("expected a scalar, vector or matrix"))
}

/// The components of a scalar (one) or vector, as bits.
fn components(v: &Val) -> R<Vec<u64>> {
    match v {
        Val::Bits(b) => Ok(vec![*b]),
        Val::Agg(items) => items.iter().map(bits).collect(),
        _ => Err(shape_error()),
    }
}

fn vector(items: Vec<u64>) -> Val {
    if items.len() == 1 {
        Val::Bits(items[0])
    } else {
        Val::Agg(items.into_iter().map(Val::Bits).collect())
    }
}

pub(crate) fn binary_int(op: BinaryOp, s: Scalar, a: u64, b: u64) -> u64 {
    let m = mask(s);
    let w = s.bits() as u64;
    let (a, b) = (a & m, b & m);
    let (sa, sb) = (sext(a, s), sext(b, s));
    let r = match op {
        BinaryOp::Add => a.wrapping_add(b),
        BinaryOp::Sub => a.wrapping_sub(b),
        BinaryOp::Mul => a.wrapping_mul(b),
        BinaryOp::SDiv => {
            if b == 0 {
                0
            } else {
                sa.wrapping_div(sb) as u64
            }
        }
        BinaryOp::UDiv => a.checked_div(b).unwrap_or(0),
        BinaryOp::SRem => {
            if b == 0 {
                0
            } else {
                sa.wrapping_rem(sb) as u64
            }
        }
        BinaryOp::URem => a.checked_rem(b).unwrap_or(0),
        BinaryOp::And => a & b,
        BinaryOp::Or => a | b,
        BinaryOp::Xor => a ^ b,
        BinaryOp::Shl => a << (b % w),
        BinaryOp::LShr => a >> (b % w),
        BinaryOp::AShr => (sa >> (b % w)) as u64,
        BinaryOp::MulHiS => ((sa as i128 * sb as i128) >> w) as u64,
        BinaryOp::MulHiU => ((a as u128 * b as u128) >> w) as u64,
        _ => 0,
    };
    r & m
}

fn binary_float(op: BinaryOp, s: Scalar, a: u64, b: u64) -> u64 {
    let (x, y) = (to_f64(a, s), to_f64(b, s));
    let r = match op {
        BinaryOp::FAdd => x + y,
        BinaryOp::FSub => x - y,
        BinaryOp::FMul => x * y,
        BinaryOp::FDiv => x / y,
        BinaryOp::FRem => math::fmod(x, y),
        _ => 0.0,
    };
    from_f64(r, s)
}

fn icmp(cc: IntCmp, s: Scalar, a: u64, b: u64) -> bool {
    let (a, b) = (a & mask(s), b & mask(s));
    let (sa, sb) = (sext(a, s), sext(b, s));
    match cc {
        IntCmp::Eq => a == b,
        IntCmp::Ne => a != b,
        IntCmp::Slt => sa < sb,
        IntCmp::Sle => sa <= sb,
        IntCmp::Sgt => sa > sb,
        IntCmp::Sge => sa >= sb,
        IntCmp::Ult => a < b,
        IntCmp::Ule => a <= b,
        IntCmp::Ugt => a > b,
        IntCmp::Uge => a >= b,
    }
}

fn fcmp(cc: FloatCmp, s: Scalar, a: u64, b: u64) -> bool {
    let (x, y) = (to_f64(a, s), to_f64(b, s));
    let nan = x.is_nan() || y.is_nan();
    match cc {
        FloatCmp::Oeq => !nan && x == y,
        FloatCmp::One => !nan && x != y,
        FloatCmp::Olt => !nan && x < y,
        FloatCmp::Ole => !nan && x <= y,
        FloatCmp::Ogt => !nan && x > y,
        FloatCmp::Oge => !nan && x >= y,
        FloatCmp::Ord => !nan,
        FloatCmp::Uno => nan,
        FloatCmp::Ueq => nan || x == y,
        FloatCmp::Une => nan || x != y,
        FloatCmp::Ult => nan || x < y,
        FloatCmp::Ule => nan || x <= y,
        FloatCmp::Ugt => nan || x > y,
        FloatCmp::Uge => nan || x >= y,
    }
}

fn convert(op: ConvertOp, from: Scalar, to: Scalar, b: u64) -> u64 {
    let b = b & mask(from);
    let r = match op {
        ConvertOp::SExt => sext(b, from) as u64,
        ConvertOp::ZExt | ConvertOp::Trunc | ConvertOp::Bitcast => b,
        ConvertOp::FpExt | ConvertOp::FpTrunc => from_f64(to_f64(b, from), to),
        ConvertOp::SiToFp => {
            let v = sext(b, from);
            match to {
                Scalar::F32 => (v as f32).to_bits() as u64,
                Scalar::F64 => (v as f64).to_bits(),
                _ => f64_to_f16(v as f64) as u64,
            }
        }
        ConvertOp::UiToFp => match to {
            Scalar::F32 => (b as f32).to_bits() as u64,
            Scalar::F64 => (b as f64).to_bits(),
            _ => f64_to_f16(b as f64) as u64,
        },
        // Saturating, NaN to 0 (0006 §10): what Rust's `as` does.
        ConvertOp::FpToSi => {
            let x = to_f64(b, from);
            match to {
                Scalar::I16 => x as i16 as u64,
                Scalar::I32 => x as i32 as u64,
                _ => x as i64 as u64,
            }
        }
        ConvertOp::FpToUi => {
            let x = to_f64(b, from);
            match to {
                Scalar::I16 => x as u16 as u64,
                Scalar::I32 => x as u32 as u64,
                _ => x as u64,
            }
        }
    };
    r & mask(to)
}

fn fmin(x: f64, y: f64) -> f64 {
    if x.is_nan() {
        y
    } else if y.is_nan() || x <= y {
        x
    } else {
        y
    }
}

fn fmax(x: f64, y: f64) -> f64 {
    if x.is_nan() {
        y
    } else if y.is_nan() || x >= y {
        x
    } else {
        y
    }
}

fn math_float1(op: MathOp, x: f64) -> f64 {
    use MathOp::*;
    match op {
        Sqrt => x.sqrt(),
        Rsqrt => 1.0 / x.sqrt(),
        Rcp => 1.0 / x,
        Exp2 => math::exp2(x),
        Log2 => math::log2(x),
        Sin => math::sin(x),
        Cos => math::cos(x),
        Tan => math::tan(x),
        Asin => math::asin(x),
        Acos => math::acos(x),
        Atan => math::atan(x),
        Sinh => math::sinh(x),
        Cosh => math::cosh(x),
        Tanh => math::tanh(x),
        Exp => math::exp(x),
        Log => math::ln(x),
        Floor => x.floor(),
        Ceil => x.ceil(),
        FTrunc => x.trunc(),
        RoundEven => x.round_ties_even(),
        Fract => x - x.floor(),
        Abs => x.abs(),
        Sign => {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else if x.is_nan() {
                0.0
            } else {
                x
            }
        }
        _ => f64::NAN,
    }
}

fn first_bit_hi(v: u64, w: u32) -> u64 {
    if v == 0 {
        u64::MAX
    } else {
        (63 - v.leading_zeros() as u64).min(w as u64 - 1)
    }
}

fn math_int(op: MathOp, s: Scalar, args: &[u64]) -> u64 {
    use MathOp::*;
    let m = mask(s);
    let w = s.bits();
    let a = args[0] & m;
    let sa = sext(a, s);
    let r = match op {
        Abs => sa.wrapping_abs() as u64,
        Sign => sa.signum() as u64,
        SMin => sa.min(sext(args[1], s)) as u64,
        SMax => sa.max(sext(args[1], s)) as u64,
        UMin => a.min(args[1] & m),
        UMax => a.max(args[1] & m),
        Popcount => a.count_ones() as u64,
        FirstBitLo => {
            if a == 0 {
                u64::MAX
            } else {
                a.trailing_zeros() as u64
            }
        }
        FirstBitHiU => first_bit_hi(a, w),
        FirstBitHiS => {
            // The highest bit that differs from the sign.
            let v = if sa < 0 { !a & m } else { a };
            first_bit_hi(v, w)
        }
        BitReverse => a.reverse_bits() >> (64 - w),
        BitfieldExtractS | BitfieldExtractU => {
            let off = (args[1] as u32 as u64).min(w as u64);
            let count = (args[2] as u32 as u64).min(w as u64 - off);
            if count == 0 {
                0
            } else {
                let fmask = if count >= 64 {
                    u64::MAX
                } else {
                    (1u64 << count) - 1
                };
                let field = (a >> off) & fmask;
                if op == BitfieldExtractS && (field >> (count - 1)) & 1 == 1 {
                    field | !fmask
                } else {
                    field
                }
            }
        }
        BitfieldInsert => {
            let off = (args[2] as u32 as u64).min(w as u64);
            let count = (args[3] as u32 as u64).min(w as u64 - off);
            if count == 0 {
                a
            } else {
                let field = if count >= 64 {
                    u64::MAX
                } else {
                    (1u64 << count) - 1
                } << off;
                (a & !field) | ((args[1] << off) & field)
            }
        }
        _ => 0,
    };
    r & m
}

/// Float vectors as `f64` components.
fn fvec(v: &Val, s: Scalar) -> R<Vec<f64>> {
    Ok(components(v)?.into_iter().map(|b| to_f64(b, s)).collect())
}

fn fvec_val(xs: &[f64], s: Scalar) -> Val {
    vector(xs.iter().map(|&x| from_f64(x, s)).collect())
}

fn dot_f(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn math(m: &Module, op: MathOp, args: &[Val], tys: &[Type]) -> R<Val> {
    use MathOp::*;
    let s = scalar_of(m, tys[0])?;
    if s.is_int() {
        return match op {
            Dot => {
                let (a, b) = (components(&args[0])?, components(&args[1])?);
                let sum = a
                    .iter()
                    .zip(&b)
                    .fold(0u64, |acc, (x, y)| acc.wrapping_add(x.wrapping_mul(*y)));
                Ok(Val::Bits(sum & mask(s)))
            }
            _ => match args.len() {
                1 => map1(&args[0], &mut |a| math_int(op, s, &[a])),
                2 => map2(&args[0], &args[1], &mut |a, b| math_int(op, s, &[a, b])),
                3 => {
                    // bitfield_extract: the offset and count are scalars.
                    let (o, c) = (bits(&args[1])?, bits(&args[2])?);
                    map1(&args[0], &mut |a| math_int(op, s, &[a, o, c]))
                }
                _ => {
                    let (o, c) = (bits(&args[2])?, bits(&args[3])?);
                    map2(&args[0], &args[1], &mut |a, b| {
                        math_int(op, s, &[a, b, o, c])
                    })
                }
            },
        };
    }
    let f = |b: u64| to_f64(b, s);
    let back = |x: f64| from_f64(x, s);
    Ok(match op {
        IsNan => map1(&args[0], &mut |a| f(a).is_nan() as u64)?,
        IsInf => map1(&args[0], &mut |a| f(a).is_infinite() as u64)?,
        Abs => map1(&args[0], &mut |a| a & (mask(s) >> 1))?,
        FMin => map2(&args[0], &args[1], &mut |a, b| back(fmin(f(a), f(b))))?,
        FMax => map2(&args[0], &args[1], &mut |a, b| back(fmax(f(a), f(b))))?,
        Pow => map2(&args[0], &args[1], &mut |a, b| back(math::pow(f(a), f(b))))?,
        Atan2 => map2(&args[0], &args[1], &mut |a, b| {
            back(math::atan2(f(a), f(b)))
        })?,
        Step => map2(&args[0], &args[1], &mut |e, x| {
            back(if f(x) >= f(e) { 1.0 } else { 0.0 })
        })?,
        Fma => map3(&args[0], &args[1], &args[2], &mut |a, b, c| {
            back(f(a).mul_add(f(b), f(c)))
        })?,
        Smoothstep => map3(&args[0], &args[1], &args[2], &mut |e0, e1, x| {
            let (e0, e1, x) = (f(e0), f(e1), f(x));
            let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
            back(t * t * (3.0 - 2.0 * t))
        })?,
        Dot => {
            let (a, b) = (fvec(&args[0], s)?, fvec(&args[1], s)?);
            Val::Bits(back(dot_f(&a, &b)))
        }
        Length => {
            let a = fvec(&args[0], s)?;
            Val::Bits(back(dot_f(&a, &a).sqrt()))
        }
        Distance => {
            let (a, b) = (fvec(&args[0], s)?, fvec(&args[1], s)?);
            let d: Vec<f64> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
            Val::Bits(back(dot_f(&d, &d).sqrt()))
        }
        Normalize => {
            let a = fvec(&args[0], s)?;
            let len = dot_f(&a, &a).sqrt();
            fvec_val(&a.iter().map(|x| x / len).collect::<Vec<_>>(), s)
        }
        Cross => {
            let (a, b) = (fvec(&args[0], s)?, fvec(&args[1], s)?);
            if a.len() != 3 || b.len() != 3 {
                return Err(shape_error());
            }
            fvec_val(
                &[
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ],
                s,
            )
        }
        Reflect => {
            let (i, n) = (fvec(&args[0], s)?, fvec(&args[1], s)?);
            let d = 2.0 * dot_f(&n, &i);
            fvec_val(
                &i.iter().zip(&n).map(|(i, n)| i - d * n).collect::<Vec<_>>(),
                s,
            )
        }
        Refract => {
            let (i, n) = (fvec(&args[0], s)?, fvec(&args[1], s)?);
            let eta = f(bits(&args[2])?);
            let d = dot_f(&n, &i);
            let k = 1.0 - eta * eta * (1.0 - d * d);
            if k < 0.0 {
                fvec_val(&vec![0.0; i.len()], s)
            } else {
                let c = eta * d + k.sqrt();
                fvec_val(
                    &i.iter()
                        .zip(&n)
                        .map(|(i, n)| eta * i - c * n)
                        .collect::<Vec<_>>(),
                    s,
                )
            }
        }
        FaceForward => {
            let (n, i, r) = (fvec(&args[0], s)?, fvec(&args[1], s)?, fvec(&args[2], s)?);
            let sign = if dot_f(&r, &i) < 0.0 { 1.0 } else { -1.0 };
            fvec_val(&n.iter().map(|x| sign * x).collect::<Vec<_>>(), s)
        }
        _ => map1(&args[0], &mut |a| back(math_float1(op, f(a))))?,
    })
}

/// The column-major matrix `v` as columns of `f64`.
fn columns(v: &Val, s: Scalar) -> R<Vec<Vec<f64>>> {
    parts(v)?.iter().map(|c| fvec(c, s)).collect()
}

fn matrix_val(cols: &[Vec<f64>], s: Scalar) -> Val {
    Val::Agg(
        cols.iter()
            .map(|c| Val::Agg(c.iter().map(|&x| Val::Bits(from_f64(x, s))).collect()))
            .collect(),
    )
}

fn matmul(m: &Module, args: &[Val], tys: &[Type]) -> R<Val> {
    let s = scalar_of(m, tys[0])?;
    let is_mat = |t: Type| matches!(m.data(t), TypeData::Matrix { .. });
    match (is_mat(tys[0]), is_mat(tys[1])) {
        (true, true) => {
            let (a, b) = (columns(&args[0], s)?, columns(&args[1], s)?);
            let rows = a.first().map_or(0, Vec::len);
            let out: Vec<Vec<f64>> = b
                .iter()
                .map(|bc| {
                    (0..rows)
                        .map(|r| (0..a.len()).map(|k| a[k][r] * bc[k]).sum())
                        .collect()
                })
                .collect();
            Ok(matrix_val(&out, s))
        }
        (true, false) => {
            let (a, v) = (columns(&args[0], s)?, fvec(&args[1], s)?);
            let rows = a.first().map_or(0, Vec::len);
            let out: Vec<f64> = (0..rows)
                .map(|r| (0..a.len()).map(|k| a[k][r] * v[k]).sum())
                .collect();
            Ok(fvec_val(&out, s))
        }
        (false, true) => {
            let (v, b) = (fvec(&args[0], s)?, columns(&args[1], s)?);
            let out: Vec<f64> = b.iter().map(|c| dot_f(&v, c)).collect();
            Ok(fvec_val(&out, s))
        }
        _ => Err(shape_error()),
    }
}

/// Evaluates a side-effect-free operation.
pub(crate) fn eval(
    m: &Module,
    op: &Op,
    args: &[Val],
    tys: &[Type],
    result: Option<Type>,
) -> R<Val> {
    let arg = |i: usize| args.get(i).ok_or_else(shape_error);
    match op {
        Op::Binary(b) => {
            let s = scalar_of(m, tys[0])?;
            if b.is_float() {
                map2(arg(0)?, arg(1)?, &mut |x, y| binary_float(*b, s, x, y))
            } else {
                map2(arg(0)?, arg(1)?, &mut |x, y| binary_int(*b, s, x, y))
            }
        }
        Op::Unary(u) => {
            let s = scalar_of(m, tys[0])?;
            let w = s.bits();
            map1(arg(0)?, &mut |x| match u {
                UnaryOp::Neg => 0u64.wrapping_sub(x) & mask(s),
                UnaryOp::Not => !x & mask(s),
                UnaryOp::FNeg => x ^ (1u64 << (w - 1)),
            })
        }
        Op::Icmp(cc) => {
            let s = scalar_of(m, tys[0])?;
            map2(arg(0)?, arg(1)?, &mut |x, y| icmp(*cc, s, x, y) as u64)
        }
        Op::Fcmp(cc) => {
            let s = scalar_of(m, tys[0])?;
            map2(arg(0)?, arg(1)?, &mut |x, y| fcmp(*cc, s, x, y) as u64)
        }
        Op::Select => match arg(0)? {
            Val::Bits(c) => Ok(if *c & 1 == 1 { arg(1)? } else { arg(2)? }.clone()),
            Val::Agg(cs) => {
                let (a, b) = (parts(arg(1)?)?, parts(arg(2)?)?);
                if a.len() != cs.len() || b.len() != cs.len() {
                    return Err(shape_error());
                }
                Ok(Val::Agg(
                    cs.iter()
                        .zip(a.iter().zip(b))
                        .map(|(c, (x, y))| {
                            Ok(if bits(c)? & 1 == 1 {
                                x.clone()
                            } else {
                                y.clone()
                            })
                        })
                        .collect::<R<_>>()?,
                ))
            }
            _ => Err(shape_error()),
        },
        Op::Convert(c) => {
            let from = scalar_of(m, tys[0])?;
            let to = scalar_of(m, result.ok_or_else(shape_error)?)?;
            map1(arg(0)?, &mut |x| convert(*c, from, to, x))
        }
        Op::Math(op) => math(m, *op, args, tys),
        Op::Splat => {
            let t = result.ok_or_else(shape_error)?;
            let n = m.width(t).ok_or_else(shape_error)? as usize;
            Ok(Val::Agg(vec![arg(0)?.clone(); n]))
        }
        Op::Extract(i) | Op::ExtractField(i) | Op::ExtractElem(i) | Op::MatColumn(i) => {
            parts(arg(0)?)?
                .get(*i as usize)
                .cloned()
                .ok_or_else(shape_error)
        }
        Op::Insert(i) | Op::InsertField(i) | Op::InsertElem(i) => {
            let mut items = parts(arg(0)?)?.to_vec();
            let slot = items.get_mut(*i as usize).ok_or_else(shape_error)?;
            *slot = arg(1)?.clone();
            Ok(Val::Agg(items))
        }
        Op::Shuffle(lanes) => {
            let mut all = parts(arg(0)?)?.to_vec();
            all.extend_from_slice(parts(arg(1)?)?);
            let picked: Vec<Val> = lanes
                .iter()
                .map(|&l| all.get(l as usize).cloned().ok_or_else(shape_error))
                .collect::<R<_>>()?;
            Ok(if picked.len() == 1 {
                picked.into_iter().next().unwrap_or(Val::Bits(0))
            } else {
                Val::Agg(picked)
            })
        }
        Op::ExtractDyn => {
            let i = bits(arg(1)?)?;
            Ok(parts(arg(0)?)?
                .get(i as usize)
                .cloned()
                .unwrap_or(Val::Bits(0)))
        }
        Op::InsertDyn => {
            let i = bits(arg(1)?)?;
            let mut items = parts(arg(0)?)?.to_vec();
            if let Some(slot) = items.get_mut(i as usize) {
                *slot = arg(2)?.clone();
            }
            Ok(Val::Agg(items))
        }
        Op::MatMul => matmul(m, args, tys),
        Op::Transpose => {
            let cols = parts(arg(0)?)?;
            let rows = cols.first().map_or(Ok(0), |c| parts(c).map(<[Val]>::len))?;
            let mut out = Vec::with_capacity(rows);
            for r in 0..rows {
                let mut col = Vec::with_capacity(cols.len());
                for c in cols {
                    col.push(parts(c)?.get(r).cloned().ok_or_else(shape_error)?);
                }
                out.push(Val::Agg(col));
            }
            Ok(Val::Agg(out))
        }
        Op::MatRow(i) => {
            let cols = parts(arg(0)?)?;
            let row: Vec<Val> = cols
                .iter()
                .map(|c| parts(c)?.get(*i as usize).cloned().ok_or_else(shape_error))
                .collect::<R<_>>()?;
            Ok(Val::Agg(row))
        }
        Op::MatExtract(r, c) => parts(arg(0)?)?
            .get(*c as usize)
            .and_then(|col| col.parts()?.get(*r as usize).cloned())
            .ok_or_else(shape_error),
        Op::Construct => {
            let t = result.ok_or_else(shape_error)?;
            match m.data(t) {
                TypeData::Vector(..) => {
                    let mut items = Vec::new();
                    for a in args {
                        match a {
                            Val::Bits(_) => items.push(a.clone()),
                            Val::Agg(parts) => items.extend(parts.iter().cloned()),
                            _ => return Err(shape_error()),
                        }
                    }
                    Ok(Val::Agg(items))
                }
                _ => Ok(Val::Agg(args.to_vec())),
            }
        }
        _ => Err(ExecError::new("not a value operation")),
    }
}
