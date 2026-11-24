//! 定点十进制：`i128` 尾数加十进制小数位数。
//!
//! 金额路径不用浮点（和 Pixiu 的规则一致）。加、减、乘是精确的；除法按指定小数位数四舍六入五成双
//! （half-even），与 `live_report.py` 的 `Decimal.quantize(..., ROUND_HALF_EVEN)` 一致。
//! 只有交给图表时才转成 `f64`（[`Fx::to_f64`]）。

use std::cmp::Ordering;
use std::fmt;

/// 计算中间结果（bps、中间价、均值）使用的小数位数。`live_report.py` 的精度是 50 位有效数字；
/// 18 位小数足以让 3 位、6 位的最终舍入结果与它一致（见 `docs/design.md` 5.1）。
pub const WORK_SCALE: u32 = 18;

/// 允许的最大小数位数（10^38 已接近 `i128` 上限）。
const MAX_SCALE: u32 = 36;

/// 一个十进制数：值 = `v / 10^s`。
#[derive(Clone, Copy, Debug)]
pub struct Fx {
    v: i128,
    s: u32,
}

/// 十进制运算溢出或输入非法。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FxError(pub String);

impl fmt::Display for FxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "十进制运算错误：{}", self.0)
    }
}

impl std::error::Error for FxError {}

fn pow10(n: u32) -> Result<i128, FxError> {
    10i128
        .checked_pow(n)
        .ok_or_else(|| FxError(format!("10^{n} 溢出")))
}

impl Fx {
    pub const ZERO: Fx = Fx { v: 0, s: 0 };

    /// 尾数加小数位数，例如 `Fx::new(8060540000000, 8)` 是 80605.4。
    pub fn new(mantissa: i128, scale: u32) -> Fx {
        Fx {
            v: mantissa,
            s: scale,
        }
    }

    pub fn from_int(n: i64) -> Fx {
        Fx {
            v: i128::from(n),
            s: 0,
        }
    }

    /// 解析十进制字符串（`-12.340`、`80715.4`、`1e-3` 不支持）。
    pub fn parse(text: &str) -> Result<Fx, FxError> {
        let t = text.trim();
        let (neg, body) = match t.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        let (int_part, frac_part) = match body.split_once('.') {
            Some((a, b)) => (a, b),
            None => (body, ""),
        };
        let ok = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if (int_part.is_empty() && frac_part.is_empty()) || !ok(int_part) || !ok(frac_part) {
            return Err(FxError(format!("不是十进制数：{text:?}")));
        }
        let s = u32::try_from(frac_part.len()).map_err(|_| FxError("小数位过多".into()))?;
        if s > MAX_SCALE {
            return Err(FxError(format!("小数位过多：{text:?}")));
        }
        let digits = format!("{int_part}{frac_part}");
        let digits = if digits.is_empty() { "0" } else { &digits };
        let v: i128 = digits
            .parse()
            .map_err(|_| FxError(format!("数值过大：{text:?}")))?;
        Ok(Fx {
            v: if neg { -v } else { v },
            s,
        })
    }

    pub fn scale(self) -> u32 {
        self.s
    }

    pub fn is_zero(self) -> bool {
        self.v == 0
    }

    pub fn signum(self) -> i32 {
        match self.v.cmp(&0) {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }
    }

    pub fn abs(self) -> Fx {
        Fx {
            v: self.v.abs(),
            s: self.s,
        }
    }

    pub fn negated(self) -> Fx {
        Fx {
            v: -self.v,
            s: self.s,
        }
    }

    /// 把小数位数提高到 `s`（精确，不舍入）。
    pub fn rescale_up(self, s: u32) -> Result<Fx, FxError> {
        if s <= self.s {
            return Ok(self);
        }
        let k = pow10(s - self.s)?;
        let v = self
            .v
            .checked_mul(k)
            .ok_or_else(|| FxError("提高小数位数时溢出".into()))?;
        Ok(Fx { v, s })
    }

    fn aligned(a: Fx, b: Fx) -> Result<(i128, i128, u32), FxError> {
        let s = a.s.max(b.s);
        Ok((a.rescale_up(s)?.v, b.rescale_up(s)?.v, s))
    }

    pub fn plus(self, o: Fx) -> Result<Fx, FxError> {
        let (a, b, s) = Fx::aligned(self, o)?;
        let v = a.checked_add(b).ok_or_else(|| FxError("加法溢出".into()))?;
        Ok(Fx { v, s })
    }

    pub fn minus(self, o: Fx) -> Result<Fx, FxError> {
        self.plus(o.negated())
    }

    pub fn times(self, o: Fx) -> Result<Fx, FxError> {
        let v = self
            .v
            .checked_mul(o.v)
            .ok_or_else(|| FxError("乘法溢出".into()))?;
        let s = self.s + o.s;
        if s > MAX_SCALE {
            // 小数位过多时先舍到工作精度，避免后续溢出。
            return Fx { v, s }.round_to(WORK_SCALE.max(self.s.max(o.s)));
        }
        Ok(Fx { v, s })
    }

    pub fn mul_int(self, n: i64) -> Result<Fx, FxError> {
        self.times(Fx::from_int(n))
    }

    /// `self / o`，结果保留 `places` 位小数，half-even。
    pub fn div(self, o: Fx, places: u32) -> Result<Fx, FxError> {
        if o.v == 0 {
            return Err(FxError("除以零".into()));
        }
        // 结果 = (a / 10^sa) / (b / 10^sb) = a * 10^(sb + places - sa) / b，再以 10^places 为分母。
        let shift = i64::from(o.s) + i64::from(places) - i64::from(self.s);
        let (num, den) = if shift >= 0 {
            let k = pow10(u32::try_from(shift).map_err(|_| FxError("位移溢出".into()))?)?;
            let n = self
                .v
                .checked_mul(k)
                .ok_or_else(|| FxError("除法分子溢出".into()))?;
            (n, o.v)
        } else {
            let k = pow10(u32::try_from(-shift).map_err(|_| FxError("位移溢出".into()))?)?;
            let d =
                o.v.checked_mul(k)
                    .ok_or_else(|| FxError("除法分母溢出".into()))?;
            (self.v, d)
        };
        Ok(Fx {
            v: div_half_even(num, den),
            s: places,
        })
    }

    pub fn div_int(self, n: i64, places: u32) -> Result<Fx, FxError> {
        self.div(Fx::from_int(n), places)
    }

    /// 舍入到 `places` 位小数（half-even）；位数已经不多于 `places` 时原样返回。
    pub fn round_to(self, places: u32) -> Result<Fx, FxError> {
        if self.s <= places {
            return Ok(self);
        }
        let k = pow10(self.s - places)?;
        Ok(Fx {
            v: div_half_even(self.v, k),
            s: places,
        })
    }

    /// 去掉尾部的 0（只改表示，不改值）。
    pub fn normalize(self) -> Fx {
        let (mut v, mut s) = (self.v, self.s);
        while s > 0 && v % 10 == 0 {
            v /= 10;
            s -= 1;
        }
        if v == 0 {
            s = 0;
        }
        Fx { v, s }
    }

    /// 与 `live_report.py` 的 `num()` 一致：精确值，无指数，无尾部 0。
    pub fn to_plain(self) -> String {
        self.normalize().to_string()
    }

    /// 与 `fixed(d, places)` 一致：舍入后打印全部 `places` 位。
    pub fn fixed(self, places: u32) -> Result<String, FxError> {
        let r = self.round_to(places)?.rescale_up(places)?;
        Ok(r.to_string())
    }

    /// 与 `fixed_unsigned_zero` 一致：舍入为 0 的值不带负号。
    pub fn fixed_unsigned_zero(self, places: u32) -> Result<String, FxError> {
        let text = self.fixed(places)?;
        Ok(match text.strip_prefix('-') {
            Some(rest) if rest.bytes().all(|b| b == b'0' || b == b'.') => rest.to_owned(),
            _ => text,
        })
    }

    /// 向零截断为整数（Python 的 `int(Decimal)`）。
    pub fn trunc(self) -> Result<i128, FxError> {
        Ok(self.v / pow10(self.s)?)
    }

    /// 仅用于图表输出。
    pub fn to_f64(self) -> f64 {
        // 图表精度足够；金额计算从不走这里。
        let p = 10f64.powi(i32::try_from(self.s).unwrap_or(i32::MAX));
        (self.v as f64) / p
    }
}

/// `n / d` 的 half-even 整数商。
fn div_half_even(n: i128, d: i128) -> i128 {
    let q = n / d;
    let r = n % d;
    if r == 0 {
        return q;
    }
    // |2r| 与 |d| 比较决定进位；相等时取偶数。
    let twice = r.unsigned_abs().saturating_mul(2);
    let dabs = d.unsigned_abs();
    let away = match twice.cmp(&dabs) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => q % 2 != 0,
    };
    if !away {
        return q;
    }
    if (n < 0) == (d < 0) { q + 1 } else { q - 1 }
}

impl fmt::Display for Fx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let neg = self.v < 0;
        let digits = self.v.unsigned_abs().to_string();
        let s = self.s as usize;
        let body = if s == 0 {
            digits
        } else if digits.len() > s {
            let (a, b) = digits.split_at(digits.len() - s);
            format!("{a}.{b}")
        } else {
            format!("0.{}{}", "0".repeat(s - digits.len()), digits)
        };
        if neg {
            write!(f, "-{body}")
        } else {
            write!(f, "{body}")
        }
    }
}

impl PartialEq for Fx {
    fn eq(&self, o: &Fx) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}

impl Eq for Fx {}

impl PartialOrd for Fx {
    fn partial_cmp(&self, o: &Fx) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Fx {
    fn cmp(&self, o: &Fx) -> Ordering {
        match Fx::aligned(*self, *o) {
            Ok((a, b, _)) => a.cmp(&b),
            // 对齐溢出时（极端位数差）退回到浮点比较；实际数据不会出现。
            Err(_) => self
                .to_f64()
                .partial_cmp(&o.to_f64())
                .unwrap_or(Ordering::Equal),
        }
    }
}

#[cfg(test)]
mod tests;
