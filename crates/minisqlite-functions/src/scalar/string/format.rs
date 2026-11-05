//! `format(FORMAT, ...)` / `printf(FORMAT, ...)` — SQLite's C-`printf`-style string
//! formatter (`spec/sqlite-doc/printf.html`, `lang_corefunc.html#format`).
//!
//! A conversion is `%[flags][width][.precision][length]type`. The format string and
//! all substitutions are assembled into a byte buffer and converted to TEXT at the
//! end, because SQLite works on bytes: a byte-measured precision may cut a multibyte
//! UTF-8 character, which SQLite emits verbatim but a `Value::Text` cannot hold, so
//! the final conversion is lossy in that (rare) case.
//!
//! Argument coercion follows SQLite: integer conversions coerce like
//! `sqlite3_value_int64` ([`to_integer`]), float conversions like
//! `sqlite3_value_double` ([`value_to_f64`]), string conversions take the text view.
//! A missing argument (too few supplied) is a NULL, i.e. 0 / 0.0 / "".
//!
//! Float conversions follow SQLite's own decimal conversion ([`fp_decode`]): at most
//! 16 significant digits (26 with `!`, printf.html §1.2), rounded half away from
//! zero, so `%.0f` of `0.5` is `1` and `%.20f` of `0.1` is `0.10000000000000000000`.

use minisqlite_expr::{to_integer, FnContext, ScalarFunction};
use minisqlite_types::{parse_real_prefix, Result, Value};

use super::text_view;

/// Caps every field-driven allocation (width padding, precision-driven zero fill /
/// float digits, `%c` repetition) to about one megabyte, so a hostile
/// `format('%2000000000d', 1)` cannot ask the allocator for gigabytes and take the
/// shared host down. Real SQLite bounds output by `SQLITE_MAX_LENGTH` (1e9); a field
/// between this cap and that bound is clamped here rather than honored, which only
/// affects pathological inputs no realistic query produces.
const SAFE_FIELD: i64 = 1_000_000;

/// `format(FORMAT, ...)` / `printf(FORMAT, ...)`.
#[derive(Debug)]
pub(super) struct Format;

impl ScalarFunction for Format {
    fn call(&self, args: &[Value], _ctx: &mut dyn FnContext) -> Result<Value> {
        debug_assert!(!args.is_empty(), "format/printf needs the format string");
        Ok(format_impl(args))
    }
}

/// The value -> f64 coercion SQLite's `sqlite3_value_double` performs: NULL is 0.0,
/// numbers pass through, text/blob take the leading real-number prefix.
fn value_to_f64(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Integer(i) => *i as f64,
        Value::Real(r) => *r,
        Value::Text(s) => parse_real_prefix(s),
        Value::Blob(b) => parse_real_prefix(&String::from_utf8_lossy(b)),
    }
}

/// The parsed elements of one `%…` conversion.
#[derive(Default)]
struct Spec {
    minus: bool,          // '-'  left-justify
    plus: bool,           // '+'  force sign on positives
    space: bool,          // ' '  space before positives
    zero: bool,           // '0'  zero-pad to width
    alt: bool,            // '#'  alternate form 1
    comma: bool,          // ','  thousands separators (base-10 numerics)
    alt2: bool,           // '!'  alternate form 2 (char-measured width/precision)
    width: usize,             // minimum field width, clamped to [0, SAFE_FIELD]
    precision: Option<usize>, // None = absent; normalized to >= 0 once at parse time
}

fn format_impl(args: &[Value]) -> Value {
    // args[0] is the format string (AtLeast(1) guarantees it exists).
    if args[0].is_null() {
        return Value::Null;
    }
    let fmt = text_view(&args[0]);
    let bytes = fmt.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 16);
    let mut argi = 1usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            // Copy the whole literal run up to the next '%' in one shot rather than
            // byte-by-byte.
            let start = i;
            while i < bytes.len() && bytes[i] != b'%' {
                i += 1;
            }
            out.extend_from_slice(&bytes[start..i]);
            continue;
        }
        match parse_and_render(bytes, i, args, &mut argi, &mut out) {
            Some(next) => i = next,
            // An unrecognized conversion (SQLite's etINVALID) stops processing and
            // returns what has been produced so far.
            None => break,
        }
    }
    super::text_from_bytes(out)
}

/// The next argument, or a NULL if the format asks for more arguments than were
/// supplied (SQLite treats a missing argument as NULL). Always advances the index.
fn next_arg<'a>(args: &'a [Value], idx: &mut usize) -> &'a Value {
    const NULL: Value = Value::Null;
    let v = args.get(*idx).unwrap_or(&NULL);
    *idx += 1;
    v
}

/// Scan a run of ASCII digits into an `i64`, saturating rather than overflowing.
/// Returns `(value, index_after_digits)`; no digits yields `(0, start)`.
fn parse_uint(bytes: &[u8], mut i: usize) -> (i64, usize) {
    let mut v: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((bytes[i] - b'0') as i64);
        i += 1;
    }
    (v, i)
}

/// Parse the conversion beginning at `bytes[start] == b'%'`, render it into `out`,
/// and return the index just past it. Returns `None` for an unknown type (caller
/// stops), matching SQLite's behavior of aborting the format on an invalid field.
fn parse_and_render(
    bytes: &[u8],
    start: usize,
    args: &[Value],
    argi: &mut usize,
    out: &mut Vec<u8>,
) -> Option<usize> {
    let n = bytes.len();
    let mut i = start + 1;
    let mut spec = Spec::default();

    // Flags (any order, repeats allowed).
    while i < n {
        match bytes[i] {
            b'-' => spec.minus = true,
            b'+' => spec.plus = true,
            b' ' => spec.space = true,
            b'0' => spec.zero = true,
            b'#' => spec.alt = true,
            b',' => spec.comma = true,
            b'!' => spec.alt2 = true,
            _ => break,
        }
        i += 1;
    }

    // Width: a '*' reads the width from the next argument (negative => left-justify
    // with the absolute value); otherwise a literal digit run.
    if i < n && bytes[i] == b'*' {
        let w = to_integer(next_arg(args, argi));
        if w < 0 {
            spec.minus = true;
            spec.width = w.unsigned_abs().min(SAFE_FIELD as u64) as usize;
        } else {
            spec.width = w.min(SAFE_FIELD) as usize;
        }
        i += 1;
    } else {
        let (w, ni) = parse_uint(bytes, i);
        spec.width = w.min(SAFE_FIELD) as usize;
        i = ni;
    }

    // Precision: '.' then a '*' (from the next argument; negative => absent) or a
    // digit run ('.' with no digits means precision 0).
    if i < n && bytes[i] == b'.' {
        i += 1;
        if i < n && bytes[i] == b'*' {
            let p = to_integer(next_arg(args, argi));
            // A negative '*' precision means "absent"; otherwise normalize to a
            // non-negative `usize` here so no downstream consumer re-clamps a signed
            // value (the invariant "precision >= 0 when Some" is type-enforced).
            spec.precision = if p < 0 { None } else { Some(p as usize) };
            i += 1;
        } else {
            let (p, ni) = parse_uint(bytes, i);
            spec.precision = Some(p as usize);
            i = ni;
        }
    }

    // Length modifiers are ignored: format() always uses 64-bit values.
    while i < n && bytes[i] == b'l' {
        i += 1;
    }

    // Type.
    if i >= n {
        return None; // trailing '%' with no type: abort like an invalid field
    }
    let ty = bytes[i];
    i += 1;

    if render_conversion(ty, &spec, args, argi, out) {
        Some(i)
    } else {
        None
    }
}

/// Render one conversion of type `ty`. Returns `false` for an unrecognized type.
fn render_conversion(
    ty: u8,
    spec: &Spec,
    args: &[Value],
    argi: &mut usize,
    out: &mut Vec<u8>,
) -> bool {
    match ty {
        b'%' => push_padded_string(out, spec, b"%"),
        b'd' | b'i' => {
            let n = to_integer(next_arg(args, argi));
            render_signed(out, spec, n);
        }
        b'u' => {
            let u = to_integer(next_arg(args, argi)) as u64;
            render_unsigned(out, spec, u, Radix::Dec, b'u');
        }
        b'x' => {
            let u = to_integer(next_arg(args, argi)) as u64;
            render_unsigned(out, spec, u, Radix::Hex, b'x');
        }
        b'X' => {
            let u = to_integer(next_arg(args, argi)) as u64;
            render_unsigned(out, spec, u, Radix::HexUpper, b'X');
        }
        b'o' => {
            let u = to_integer(next_arg(args, argi)) as u64;
            render_unsigned(out, spec, u, Radix::Oct, b'o');
        }
        // %p works like %x for the SQL function (there are no SQL pointers).
        b'p' => {
            let u = to_integer(next_arg(args, argi)) as u64;
            render_unsigned(out, spec, u, Radix::Hex, b'p');
        }
        b'f' | b'e' | b'E' | b'g' | b'G' => {
            let v = value_to_f64(next_arg(args, argi));
            render_float(out, spec, v, ty);
        }
        // %z is interchangeable with %s for the SQL function.
        b's' | b'z' => {
            let v = next_arg(args, argi);
            let s = text_view(v);
            let taken = precision_take(s.as_bytes(), spec);
            push_padded_string(out, spec, taken);
        }
        b'c' => render_char(out, spec, next_arg(args, argi)),
        b'q' => render_sql_escape(out, spec, next_arg(args, argi), b'\'', false),
        b'Q' => render_sql_escape(out, spec, next_arg(args, argi), b'\'', true),
        b'w' => render_sql_escape(out, spec, next_arg(args, argi), b'"', false),
        // %n is silently ignored and consumes no argument.
        b'n' => {}
        _ => return false,
    }
    true
}

// ---------------------------------------------------------------------------
// Padding
// ---------------------------------------------------------------------------

fn push_repeat(out: &mut Vec<u8>, b: u8, n: usize) {
    out.resize(out.len() + n, b);
}

/// Emit `prefix` + `digits` right- or left-justified within `spec.width`. When
/// zero-padding is permitted the fill goes between the prefix (sign / radix marker)
/// and the digits; otherwise spaces pad outside the whole value.
fn push_padded_numeric(out: &mut Vec<u8>, spec: &Spec, prefix: &str, digits: &str, allow_zero: bool) {
    let content = prefix.len() + digits.len();
    if spec.width <= content {
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(digits.as_bytes());
        return;
    }
    let pad = spec.width - content;
    if spec.minus {
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(digits.as_bytes());
        push_repeat(out, b' ', pad);
    } else if spec.zero && allow_zero {
        out.extend_from_slice(prefix.as_bytes());
        push_repeat(out, b'0', pad);
        out.extend_from_slice(digits.as_bytes());
    } else {
        push_repeat(out, b' ', pad);
        out.extend_from_slice(prefix.as_bytes());
        out.extend_from_slice(digits.as_bytes());
    }
}

/// Emit an already-built string body space-padded to `spec.width`. Width is measured
/// in bytes, or in characters when the `!` (alternate-form-2) flag is present.
fn push_padded_string(out: &mut Vec<u8>, spec: &Spec, body: &[u8]) {
    let content = if spec.alt2 { count_chars(body) } else { body.len() };
    if spec.width <= content {
        out.extend_from_slice(body);
        return;
    }
    let pad = spec.width - content;
    if spec.minus {
        out.extend_from_slice(body);
        push_repeat(out, b' ', pad);
    } else {
        push_repeat(out, b' ', pad);
        out.extend_from_slice(body);
    }
}

/// Count UTF-8 characters in a byte slice (every non-continuation byte).
fn count_chars(s: &[u8]) -> usize {
    s.iter().filter(|&&b| (b & 0xC0) != 0x80).count()
}

/// Apply a string precision: the leading `precision` bytes (default) or characters
/// (with `!`) of `s`. No precision returns all of `s`. A precision that runs past
/// the end returns the whole slice, so this never allocates.
fn precision_take<'a>(s: &'a [u8], spec: &Spec) -> &'a [u8] {
    let p = match spec.precision {
        None => return s,
        Some(p) => p,
    };
    if spec.alt2 {
        let mut count = 0;
        for (i, &b) in s.iter().enumerate() {
            if (b & 0xC0) != 0x80 {
                if count == p {
                    return &s[..i];
                }
                count += 1;
            }
        }
        s
    } else {
        &s[..p.min(s.len())]
    }
}

// ---------------------------------------------------------------------------
// Integer conversions
// ---------------------------------------------------------------------------

enum Radix {
    Dec,
    Oct,
    Hex,
    HexUpper,
}

/// The sign prefix for a numeric conversion: "-" when `neg`, else the "+"/" "/""
/// dictated by the `+` and ` ` flags. Shared by the signed-integer and float paths.
fn sign_str(neg: bool, spec: &Spec) -> &'static str {
    if neg {
        "-"
    } else if spec.plus {
        "+"
    } else if spec.space {
        " "
    } else {
        ""
    }
}

/// The integer/float precision, clamped so a huge precision cannot drive an
/// unbounded allocation. `None` stays `None` (absent).
fn clamped_precision(spec: &Spec) -> Option<usize> {
    spec.precision.map(|p| p.min(SAFE_FIELD as usize))
}

/// Zero-pad `digits` on the left to at least `prec` digits. A precision of 0 applied
/// to a zero value produces no digits at all (the C `%.0d` of 0 rule).
fn apply_int_precision(digits: &mut String, prec: Option<usize>, is_zero: bool) {
    if let Some(p) = prec {
        if p == 0 && is_zero {
            digits.clear();
        } else if digits.len() < p {
            let mut d = String::with_capacity(p);
            for _ in 0..(p - digits.len()) {
                d.push('0');
            }
            d.push_str(digits);
            *digits = d;
        }
    }
}

fn render_signed(out: &mut Vec<u8>, spec: &Spec, n: i64) {
    let neg = n < 0;
    let mut digits = n.unsigned_abs().to_string();
    let prec = clamped_precision(spec);
    apply_int_precision(&mut digits, prec, n == 0);
    if spec.comma {
        digits = group_commas(&digits);
    }
    let sign = sign_str(neg, spec);
    // A precision disables the 0 flag for integer conversions.
    let allow_zero = spec.zero && prec.is_none();
    push_padded_numeric(out, spec, sign, &digits, allow_zero);
}

fn render_unsigned(out: &mut Vec<u8>, spec: &Spec, u: u64, radix: Radix, ty: u8) {
    let mut digits = match radix {
        Radix::Dec => u.to_string(),
        Radix::Oct => format!("{u:o}"),
        Radix::Hex => format!("{u:x}"),
        Radix::HexUpper => format!("{u:X}"),
    };
    let prec = clamped_precision(spec);
    apply_int_precision(&mut digits, prec, u == 0);
    if spec.comma && matches!(radix, Radix::Dec) {
        digits = group_commas(&digits);
    }
    // Alternate form: 0x / 0X before hex, a leading 0 before octal (never for 0).
    let prefix = if spec.alt {
        match ty {
            b'x' | b'p' if u != 0 => "0x",
            b'X' if u != 0 => "0X",
            b'o' if !digits.starts_with('0') => "0",
            _ => "",
        }
    } else {
        ""
    };
    let allow_zero = spec.zero && prec.is_none();
    push_padded_numeric(out, spec, prefix, &digits, allow_zero);
}

/// Insert `,` every three digits from the right of an all-digit string.
fn group_commas(digits: &str) -> String {
    let b = digits.as_bytes();
    let len = b.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, &d) in b.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Floating-point conversions
// ---------------------------------------------------------------------------

fn render_float(out: &mut Vec<u8>, spec: &Spec, value: f64, ty: u8) {
    let zero_inf = value.is_infinite() && spec.zero;
    if !value.is_finite() && !zero_inf {
        // NaN renders as `NaN` (`null` with the 0 flag) and carries no sign; Inf
        // renders as `Inf` with the usual sign flags. Neither is zero-padded.
        let (sign, body) = if value.is_nan() {
            ("", if spec.zero { "null" } else { "NaN" })
        } else {
            (sign_str(value < 0.0, spec), "Inf")
        };
        push_padded_numeric(out, spec, sign, body, false);
        return;
    }

    // The steps below follow the float case of SQLite's `sqlite3_str_vappendf`.
    let mut precision = spec.precision.unwrap_or(6).min(SAFE_FIELD as usize) as i32;
    let general = matches!(ty, b'g' | b'G');
    let round = match ty {
        b'f' => -precision,
        b'g' | b'G' => {
            precision = precision.max(1);
            precision
        }
        _ => precision + 1,
    };
    // printf.html §1.2: at most 16 significant digits, 26 with the `!` flag.
    let max_digits = if spec.alt2 { 26 } else { 16 };
    // With the 0 flag, Inf prints as the digit 9 followed by 999 zeros.
    let (digits, point) = if zero_inf {
        (vec![b'9'], 1000)
    } else {
        fp_decode(value.abs(), round, max_digits)
    };
    // `-0.0` is not below zero, so it prints without a sign, as in SQLite. With `#`
    // and without `+`, a `%f` value that rounds to zero drops its minus sign.
    let neg = value < 0.0 && !(spec.alt && !spec.plus && ty == b'f' && point <= round);

    let mut exp_form = matches!(ty, b'e' | b'E');
    let strip_zeros = if general {
        precision -= 1;
        let exp = point - 1;
        if exp < -4 || exp > precision {
            exp_form = true;
        } else {
            precision -= exp;
        }
        !spec.alt
    } else {
        spec.alt2
    };
    let has_point = precision > 0 || spec.alt || spec.alt2;
    let mut e2 = if exp_form { 0 } else { point - 1 };

    let mut body = String::new();
    // The significant digits in order, then zeros once they run out.
    let mut used = 0;
    let mut next_digit = || {
        let d = digits.get(used).copied().unwrap_or(b'0');
        used += 1;
        d as char
    };
    if e2 < 0 {
        body.push('0');
    } else {
        while e2 >= 0 {
            body.push(next_digit());
            if spec.comma && e2 % 3 == 0 && e2 > 1 {
                body.push(',');
            }
            e2 -= 1;
        }
    }
    if has_point {
        body.push('.');
    }
    // Zeros between the point and the first significant digit.
    e2 += 1;
    while e2 < 0 && precision > 0 {
        body.push('0');
        precision -= 1;
        e2 += 1;
    }
    for _ in 0..precision {
        body.push(next_digit());
    }
    if strip_zeros && has_point {
        body.truncate(body.trim_end_matches('0').len());
        if body.ends_with('.') {
            if spec.alt2 {
                body.push('0');
            } else {
                body.pop();
            }
        }
    }
    if exp_form {
        let exp = point - 1;
        body.push(if ty.is_ascii_uppercase() { 'E' } else { 'e' });
        body.push(if exp < 0 { '-' } else { '+' });
        body.push_str(&format!("{:02}", exp.unsigned_abs()));
    }

    push_padded_numeric(out, spec, sign_str(neg, spec), &body, spec.zero);
}

/// Decimal digits of a finite `r >= 0`, a port of SQLite's `sqlite3FpDecode` (the
/// double-double path it takes when `long double` is no wider than `double`).
/// Returns the significant digits, without trailing zeros, and the position of the
/// decimal point: the value is `0.DIGITS * 10^point`.
///
/// `round > 0` rounds to that many significant digits; `round <= 0` rounds to
/// `-round` digits after the decimal point. Either way at most `max_digits`
/// significant digits are kept. Rounding is half away from zero on the decimal
/// digits, so `0.5` rounded to an integer is `1` and `0.125` to two places `0.13`.
// The constants are copied from SQLite as written, digits beyond f64 included.
#[allow(clippy::excessive_precision)]
fn fp_decode(r: f64, mut round: i32, max_digits: i32) -> (Vec<u8>, i32) {
    if r == 0.0 {
        return (vec![b'0'], 1);
    }
    // Scale r by powers of ten into [~9.2e17, ~9.2e18] in double-double arithmetic,
    // then read off the integer: about 19 significant digits. The second constant
    // of each pair is the error of the first.
    let mut rr = [r, 0.0];
    let mut exp = 0;
    if rr[0] > 9.223372036854774784e+18 {
        while rr[0] > 9.223372036854774784e+118 {
            exp += 100;
            dekker_mul2(&mut rr, 1.0e-100, -1.99918998026028836196e-117);
        }
        while rr[0] > 9.223372036854774784e+28 {
            exp += 10;
            dekker_mul2(&mut rr, 1.0e-10, -3.6432197315497741579e-27);
        }
        while rr[0] > 9.223372036854774784e+18 {
            exp += 1;
            dekker_mul2(&mut rr, 1.0e-01, -5.5511151231257827021e-18);
        }
    } else {
        while rr[0] < 9.223372036854774784e-83 {
            exp -= 100;
            dekker_mul2(&mut rr, 1.0e+100, -1.5902891109759918046e+83);
        }
        while rr[0] < 9.223372036854774784e+07 {
            exp -= 10;
            dekker_mul2(&mut rr, 1.0e+10, 0.0);
        }
        while rr[0] < 9.22337203685477478e+17 {
            exp -= 1;
            dekker_mul2(&mut rr, 1.0e+01, 0.0);
        }
    }
    let v = if rr[1] < 0.0 {
        (rr[0] as u64).wrapping_sub((-rr[1]) as u64)
    } else {
        (rr[0] as u64).wrapping_add(rr[1] as u64)
    };

    let mut z = v.to_string().into_bytes();
    let mut point = z.len() as i32 + exp;
    if round <= 0 {
        round = point - round;
        // Rounding at the first digit: a leading 0 gives the carry somewhere to go.
        if round == 0 && z[0] >= b'5' {
            round = 1;
            z.insert(0, b'0');
            point += 1;
        }
    }
    let mut n = z.len();
    if round > 0 && ((round as usize) < n || n > max_digits as usize) {
        n = round.min(max_digits) as usize;
        if z[n] >= b'5' {
            let mut j = n - 1;
            loop {
                if z[j] < b'9' {
                    z[j] += 1;
                    break;
                }
                z[j] = b'0';
                if j == 0 {
                    z.insert(0, b'1');
                    n += 1;
                    point += 1;
                    break;
                }
                j -= 1;
            }
        }
    }
    z.truncate(n);
    while z.len() > 1 && z.last() == Some(&b'0') {
        z.pop();
    }
    (z, point)
}

/// Dekker's double-double product `x *= (y + yy)`, as in SQLite's `dekkerMul2`.
fn dekker_mul2(x: &mut [f64; 2], y: f64, yy: f64) {
    const HI: u64 = 0xffff_ffff_fc00_0000;
    let hx = f64::from_bits(x[0].to_bits() & HI);
    let tx = x[0] - hx;
    let hy = f64::from_bits(y.to_bits() & HI);
    let ty = y - hy;
    let p = hx * hy;
    let q = hx * ty + tx * hy;
    let c = p + q;
    let mut cc = p - c + q + tx * ty;
    cc += x[0] * yy + x[1] * y;
    x[0] = c + cc;
    x[1] = c - x[0];
    x[1] += cc;
}

// ---------------------------------------------------------------------------
// %c and the SQL-escape conversions (%q, %Q, %w)
// ---------------------------------------------------------------------------

/// `%c` — the first *character* of the argument's text, repeated `precision` times
/// when the precision exceeds 1 (a SQLite extension), then width-padded. The whole
/// leading UTF-8 scalar is emitted, so a multibyte first character (e.g. `é`) is
/// preserved rather than truncated to a lone lead byte. An empty/NULL argument yields
/// a single NUL byte, matching SQLite's `c = z ? z[0] : 0`.
fn render_char(out: &mut Vec<u8>, spec: &Spec, v: &Value) {
    let s = text_view(v);
    let mut buf = [0u8; 4];
    let ch_bytes: &[u8] = match s.chars().next() {
        Some(c) => c.encode_utf8(&mut buf).as_bytes(),
        None => &[0],
    };
    let reps = match spec.precision {
        Some(p) if p > 1 => p.min(SAFE_FIELD as usize),
        _ => 1,
    };
    let mut body = Vec::with_capacity(reps * ch_bytes.len());
    for _ in 0..reps {
        body.extend_from_slice(ch_bytes);
    }
    push_padded_string(out, spec, &body);
}

/// `%q`/`%Q`/`%w` — SQL-literal escaping. `q` (the quote byte) is doubled throughout;
/// when `wrap` is set (`%Q`) the whole thing is surrounded by `q` and a NULL argument
/// renders as the bare word `NULL`. Precision limits how much of the argument is
/// taken (before escaping); width pads the final output.
fn render_sql_escape(out: &mut Vec<u8>, spec: &Spec, v: &Value, q: u8, wrap: bool) {
    if wrap && v.is_null() {
        push_padded_string(out, spec, b"NULL");
        return;
    }
    let s = text_view(v);
    let taken = precision_take(s.as_bytes(), spec);
    let mut body: Vec<u8> = Vec::with_capacity(taken.len() + 2);
    if wrap {
        body.push(q);
    }
    for &b in taken {
        if b == q {
            body.push(q);
        }
        body.push(b);
    }
    if wrap {
        body.push(q);
    }
    push_padded_string(out, spec, &body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(fmt: &str, args: &[Value]) -> String {
        let mut all = Vec::with_capacity(args.len() + 1);
        all.push(Value::Text(fmt.to_string()));
        all.extend_from_slice(args);
        match format_impl(&all) {
            Value::Text(s) => s,
            other => panic!("expected Text, got {other:?}"),
        }
    }
    fn i(n: i64) -> Value {
        Value::Integer(n)
    }
    fn r(x: f64) -> Value {
        Value::Real(x)
    }
    fn t(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    #[test]
    fn pinned_examples() {
        assert_eq!(f("%d-%s", &[i(5), t("x")]), "5-x");
        assert_eq!(f("%.2f", &[r(3.14159)]), "3.14");
        assert_eq!(f("%05d", &[i(42)]), "00042");
        assert_eq!(f("%x", &[i(255)]), "ff");
        assert_eq!(f("%q", &[t("a'b")]), "a''b");
        assert_eq!(f("%Q", &[Value::Null]), "NULL");
    }

    #[test]
    fn null_format_is_null() {
        assert!(matches!(format_impl(&[Value::Null]), Value::Null));
        assert!(matches!(format_impl(&[Value::Null, i(1)]), Value::Null));
    }

    #[test]
    fn literal_and_percent() {
        assert_eq!(f("no substitutions", &[]), "no substitutions");
        assert_eq!(f("100%%", &[]), "100%");
        assert_eq!(f("%d%%", &[i(50)]), "50%");
    }

    #[test]
    fn integer_specifiers() {
        assert_eq!(f("%d", &[i(-7)]), "-7");
        assert_eq!(f("%i", &[i(42)]), "42");
        assert_eq!(f("%+d", &[i(5)]), "+5");
        assert_eq!(f("% d", &[i(5)]), " 5");
        assert_eq!(f("%+d", &[i(-5)]), "-5");
        // Unsigned reinterpretation of a negative.
        assert_eq!(f("%u", &[i(-1)]), "18446744073709551615");
        // Coercion: text/real coerce like sqlite3_value_int64.
        assert_eq!(f("%d", &[t("42abc")]), "42");
        assert_eq!(f("%d", &[r(3.9)]), "3");
        // Missing argument is 0.
        assert_eq!(f("%d", &[]), "0");
    }

    #[test]
    fn integer_width_precision_flags() {
        assert_eq!(f("%5d", &[i(42)]), "   42");
        assert_eq!(f("%-5d", &[i(42)]), "42   ");
        assert_eq!(f("%05d", &[i(42)]), "00042");
        assert_eq!(f("%05d", &[i(-42)]), "-0042");
        assert_eq!(f("%.4d", &[i(42)]), "0042");
        // Precision disables the 0 flag; sign then space-pad.
        assert_eq!(f("%8.4d", &[i(-42)]), "   -0042");
        // %.0d of 0 is empty.
        assert_eq!(f("%.0d", &[i(0)]), "");
        assert_eq!(f("[%.0d]", &[i(0)]), "[]");
        // width from '*', negative => left-justify.
        assert_eq!(f("%*d", &[i(5), i(42)]), "   42");
        assert_eq!(f("%*d", &[i(-5), i(42)]), "42   ");
        // precision from '*'.
        assert_eq!(f("%.*d", &[i(4), i(42)]), "0042");
    }

    #[test]
    fn hex_octal() {
        assert_eq!(f("%x", &[i(255)]), "ff");
        assert_eq!(f("%X", &[i(255)]), "FF");
        assert_eq!(f("%#x", &[i(255)]), "0xff");
        assert_eq!(f("%#X", &[i(255)]), "0XFF");
        assert_eq!(f("%08x", &[i(255)]), "000000ff");
        assert_eq!(f("%#010x", &[i(255)]), "0x000000ff");
        assert_eq!(f("%o", &[i(8)]), "10");
        assert_eq!(f("%#o", &[i(8)]), "010");
        // No 0x/0 prefix for a zero value.
        assert_eq!(f("%#x", &[i(0)]), "0");
    }

    #[test]
    fn float_f() {
        assert_eq!(f("%f", &[r(3.5)]), "3.500000");
        assert_eq!(f("%.2f", &[r(3.14159)]), "3.14");
        assert_eq!(f("%.0f", &[r(3.7)]), "4");
        // SQLite rounds half away from zero and keeps 16 significant digits.
        assert_eq!(f("%.0f", &[r(0.5)]), "1");
        assert_eq!(f("%.0f", &[r(-2.5)]), "-3");
        assert_eq!(f("%.2f", &[r(0.125)]), "0.13");
        assert_eq!(f("%.20f", &[r(0.1)]), "0.10000000000000000000");
        assert_eq!(f("%!.30f", &[r(0.1)]), "0.1000000000000000055");
        assert_eq!(f("%8.2f", &[r(3.14159)]), "    3.14");
        assert_eq!(f("%-8.2f", &[r(3.14159)]), "3.14    ");
        assert_eq!(f("%08.2f", &[r(3.14159)]), "00003.14");
        assert_eq!(f("%+.2f", &[r(3.14)]), "+3.14");
        assert_eq!(f("%.2f", &[r(-3.14159)]), "-3.14");
        // Alternate form: force the point, and suppress a negative that rounds to 0.
        assert_eq!(f("%#.0f", &[r(3.0)]), "3.");
        assert_eq!(f("%#.2f", &[r(-0.004)]), "0.00");
        // Missing arg -> 0.0.
        assert_eq!(f("%.1f", &[]), "0.0");
    }

    #[test]
    fn float_e_and_g() {
        assert_eq!(f("%e", &[r(1000.0)]), "1.000000e+03");
        assert_eq!(f("%E", &[r(1000.0)]), "1.000000E+03");
        assert_eq!(f("%.2e", &[r(0.5)]), "5.00e-01");
        assert_eq!(f("%e", &[r(1.5e300)]), "1.500000e+300");
        assert_eq!(f("%g", &[r(100000.0)]), "100000");
        assert_eq!(f("%g", &[r(1000000.0)]), "1e+06");
        assert_eq!(f("%g", &[r(0.0001)]), "0.0001");
        assert_eq!(f("%g", &[r(0.00001)]), "1e-05");
        assert_eq!(f("%.3g", &[r(3.14159)]), "3.14");
        assert_eq!(f("%g", &[r(0.0)]), "0");
        // Alternate form keeps trailing zeros.
        assert_eq!(f("%#.3g", &[r(2.0)]), "2.00");
    }

    #[test]
    fn float_non_finite() {
        assert_eq!(f("%f", &[r(f64::INFINITY)]), "Inf");
        assert_eq!(f("%f", &[r(f64::NEG_INFINITY)]), "-Inf");
        // With the 0 flag, Inf prints as 9e999 in the requested form, as in SQLite.
        assert_eq!(f("%05e", &[r(f64::INFINITY)]), "9.000000e+999");
        assert_eq!(f("%05f", &[r(f64::NEG_INFINITY)]), format!("-9{}.000000", "0".repeat(999)));
        assert_eq!(f("%0f", &[r(f64::NAN)]), "null");
    }

    #[test]
    fn strings() {
        assert_eq!(f("%s", &[t("hello")]), "hello");
        assert_eq!(f("%.2s", &[t("hello")]), "he");
        assert_eq!(f("%5s", &[t("hi")]), "   hi");
        assert_eq!(f("%-5s", &[t("hi")]), "hi   ");
        // NULL string arg is empty.
        assert_eq!(f("[%s]", &[Value::Null]), "[]");
        // Numbers coerce to text.
        assert_eq!(f("%s", &[i(42)]), "42");
        assert_eq!(f("%s", &[r(1.5)]), "1.5");
        // %z is the same as %s.
        assert_eq!(f("%z", &[t("zed")]), "zed");
        // Byte precision (default) counts bytes; the ! flag counts characters.
        assert_eq!(f("%.2s", &[t("éq")]), "é"); // first 2 bytes = the 2-byte 'é'
        assert_eq!(f("%!.2s", &[t("éq")]), "éq"); // first 2 characters
    }

    #[test]
    fn char_specifier() {
        assert_eq!(f("%c", &[t("hello")]), "h");
        assert_eq!(f("%c", &[t("")]), "\0");
        // NULL argument behaves like the empty string: a single NUL byte.
        assert_eq!(f("%c", &[Value::Null]), "\0");
        // Precision > 1 repeats.
        assert_eq!(f("%.3c", &[t("x")]), "xxx");
    }

    #[test]
    fn c_extracts_first_character_not_first_byte() {
        // printf.html: for format(), %c displays the first CHARACTER of the argument.
        // 'é' == U+00E9 == bytes C3 A9; its first character is 'é', not a lone byte.
        assert_eq!(f("%c", &[t("é")]), "é");
        // Precision > 1 repeats that whole character, not the lead byte.
        assert_eq!(f("%.3c", &[t("é")]), "ééé");
        // A leading multibyte char with trailing text still yields just that char.
        assert_eq!(f("%c", &[t("日本")]), "日");
    }

    #[test]
    fn float_alt2_forces_decimal_point_and_digit() {
        // '!' (alternate-form-2) on floats forces a decimal point AND at least one
        // fractional digit, even where the plain form would show neither.
        assert_eq!(f("%!g", &[r(3.0)]), "3.0");
        assert_eq!(f("%!.0f", &[r(3.0)]), "3.0");
        assert_eq!(f("%!.0e", &[r(3.0)]), "3.0e+00");
        // A value that already has fractional digits is unchanged by '!', and '!'
        // drops trailing zeros from %f and %e, as SQLite does.
        assert_eq!(f("%!g", &[r(3.5)]), "3.5");
        assert_eq!(f("%!f", &[r(3.5)]), "3.5");
        assert_eq!(f("%!.4e", &[r(3.5)]), "3.5e+00");
        // '!' still strips %g trailing zeros (unlike '#'), then re-adds one.
        assert_eq!(f("%!g", &[r(2.0)]), "2.0");
    }

    #[test]
    fn sql_escapes() {
        assert_eq!(f("%q", &[t("a'b'c")]), "a''b''c");
        assert_eq!(f("%Q", &[t("a'b")]), "'a''b'");
        assert_eq!(f("%Q", &[Value::Null]), "NULL");
        assert_eq!(f("%q", &[Value::Null]), ""); // %q NULL -> empty
        assert_eq!(f("%w", &[t("a\"b")]), "a\"\"b");
        assert_eq!(f("%Q", &[i(5)]), "'5'");
    }

    #[test]
    fn ignored_and_alias_conversions() {
        // %n is ignored and consumes no argument, so the following %d still sees 7.
        assert_eq!(f("a%nb%d", &[i(7)]), "ab7");
        // %p works like %x.
        assert_eq!(f("%p", &[i(255)]), "ff");
    }

    #[test]
    fn comma_grouping() {
        assert_eq!(f("%,d", &[i(2147483647)]), "2,147,483,647");
        assert_eq!(f("%,d", &[i(1000)]), "1,000");
        assert_eq!(f("%,d", &[i(999)]), "999");
    }

    #[test]
    fn too_few_arguments_are_null() {
        // Every missing argument is a NULL: 0 / 0.0 / "".
        assert_eq!(f("%d %s %f", &[]), "0  0.000000");
    }

    #[test]
    fn multiple_conversions_track_arguments() {
        assert_eq!(f("%d/%d/%d", &[i(2024), i(1), i(9)]), "2024/1/9");
        assert_eq!(f("%s=%d", &[t("k"), i(3)]), "k=3");
        // '*' width/precision consume arguments in order before the value.
        assert_eq!(f("%*.*f", &[i(10), i(2), r(3.14159)]), "      3.14");
    }
}
