//! Conformance battery for the floating-point conversions of `printf()` /
//! `format()` (`%f`, `%e`, `%E`, `%g`, `%G`).
//!
//! printf.html §1.2 limits the conversion "to 16 significant digits or 26
//! significant digits if the "!" alternate-form-2 flag is used". The page does not
//! say how a value halfway between two outputs rounds; sqlite3 rounds it half away
//! from zero. Every expectation here is the output of sqlite3 3.51.3 for the same
//! expression.

mod conformance;

use conformance::*;

#[test]
fn printf_rounds_half_away_from_zero() {
    // The first row of #13 section 4. sqlite3 rounds the decimal digits half away from
    // zero; a C-library printf rounds 0.5 and 0.125 to even.
    eval_eq(r#"printf('%.0f', 0.5)"#, text(r#"1"#));
    eval_eq(r#"printf('%.0f', 1.5)"#, text(r#"2"#));
    eval_eq(r#"printf('%.0f', 2.5)"#, text(r#"3"#));
    eval_eq(r#"printf('%.0f', 9.5)"#, text(r#"10"#));
    eval_eq(r#"printf('%.0f', 10.5)"#, text(r#"11"#));
    eval_eq(r#"printf('%.0f', -0.5)"#, text(r#"-1"#));
    eval_eq(r#"printf('%.0f', -2.5)"#, text(r#"-3"#));
    eval_eq(r#"printf('%.2f', 0.125)"#, text(r#"0.13"#));
    eval_eq(r#"printf('%.2f', 0.375)"#, text(r#"0.38"#));
    eval_eq(r#"printf('%.2f', -0.125)"#, text(r#"-0.13"#));
    eval_eq(r#"printf('%.1f', 0.25)"#, text(r#"0.3"#));
    eval_eq(r#"printf('%.3f', 1.0625)"#, text(r#"1.063"#));
    eval_eq(r#"printf('%.0e', 25.0)"#, text(r#"3e+01"#));
    eval_eq(r#"printf('%.1e', 0.25)"#, text(r#"2.5e-01"#));
    eval_eq(r#"printf('%.2e', 1.125)"#, text(r#"1.13e+00"#));
    eval_eq(r#"printf('%.2g', 0.125)"#, text(r#"0.13"#));
    eval_eq(r#"printf('%.1g', 2.5)"#, text(r#"3"#));
    eval_eq(r#"printf('%.0f', 0.4)"#, text(r#"0"#));
    eval_eq(r#"printf('%.0f', 0.49999999999999994)"#, text(r#"0"#));
    eval_eq(r#"format('%.0f', 0.5)"#, text(r#"1"#));
    eval_eq(r#"format('%.2f', 0.125)"#, text(r#"0.13"#));
}

#[test]
fn printf_rounds_the_decimal_value_not_the_literal() {
    // 2.675 and 1.005 are stored just below the half, so they round down as before.
    eval_eq(r#"printf('%.2f', 2.675)"#, text(r#"2.67"#));
    eval_eq(r#"printf('%.2f', 1.005)"#, text(r#"1.00"#));
    eval_eq(r#"printf('%.1f', 0.05)"#, text(r#"0.1"#));
    eval_eq(r#"printf('%.1f', 0.45)"#, text(r#"0.5"#));
    eval_eq(r#"printf('%.2e', 2.675)"#, text(r#"2.67e+00"#));
    eval_eq(r#"printf('%.0f', 3.7)"#, text(r#"4"#));
    eval_eq(r#"printf('%.3f', 2.0)"#, text(r#"2.000"#));
}

#[test]
fn printf_limits_floats_to_16_significant_digits() {
    // The second row of #13 section 4 and printf.html section 1.2: digits after the 16th
    // significant one print as 0.
    eval_eq(r#"printf('%.20f', 0.1)"#, text(r#"0.10000000000000000000"#));
    eval_eq(r#"printf('%.30f', 0.1)"#, text(r#"0.100000000000000000000000000000"#));
    eval_eq(r#"printf('%.15f', 0.1)"#, text(r#"0.100000000000000"#));
    eval_eq(r#"printf('%.17f', 0.1)"#, text(r#"0.10000000000000000"#));
    eval_eq(r#"printf('%.20f', 2.675)"#, text(r#"2.67500000000000000000"#));
    eval_eq(r#"printf('%.20f', 1.0/3)"#, text(r#"0.33333333333333330000"#));
    eval_eq(r#"printf('%.20e', 0.1)"#, text(r#"1.00000000000000000000e-01"#));
    eval_eq(r#"printf('%.20e', 1.0/3)"#, text(r#"3.33333333333333300000e-01"#));
    eval_eq(r#"printf('%.17g', 0.1)"#, text(r#"0.1"#));
    eval_eq(r#"printf('%.17g', 2.675)"#, text(r#"2.675"#));
    eval_eq(r#"printf('%.20g', 1.0/3)"#, text(r#"0.3333333333333333"#));
    eval_eq(r#"printf('%.20f', -0.1)"#, text(r#"-0.10000000000000000000"#));
    eval_eq(r#"printf('%.5f', 1e-5)"#, text(r#"0.00001"#));
    eval_eq(r#"printf('%.20f', 1e-5)"#, text(r#"0.00001000000000000000"#));
    eval_eq(r#"printf('%f', 1e300) = '1' || replace(printf('%.300c', 'x'), 'x', '0') || '.000000'"#, int(1));
    eval_eq(r#"printf('%.0f', 1e300) = '1' || replace(printf('%.300c', 'x'), 'x', '0')"#, int(1));
    eval_eq(r#"printf('%.2f', -1e300) = '-1' || replace(printf('%.300c', 'x'), 'x', '0') || '.00'"#, int(1));
    eval_eq(r#"printf('%.0f', 123456789012345678.0)"#, text(r#"123456789012345700"#));
    eval_eq(r#"printf('%.3e', 1e300)"#, text(r#"1.000e+300"#));
    eval_eq(r#"substr(printf('%f', 1e300), 1, 20)"#, text(r#"10000000000000000000"#));
    eval_eq(r#"printf('%g', 1e300)"#, text(r#"1e+300"#));
    eval_eq(r#"printf('%.0f', 1.7976931348623157e308) = '1797693134862316' || replace(printf('%.293c', 'x'), 'x', '0')"#, int(1));
    eval_eq(r#"printf('%.20e', 4.9406564584124654e-324)"#, text(r#"4.94065645841246500000e-324"#));
    eval_eq(r#"length(printf('%.1000f', 0.1))"#, int(1002));
    eval_eq(r#"printf('%.1000f', 0.1) = '0.1' || replace(printf('%.999c', 'x'), 'x', '0')"#, int(1));
}

#[test]
fn printf_alt2_flag_allows_26_significant_digits() {
    // With '!' the digits run out after about 19, sqlite3 drops the trailing zeros of
    // %f and %e, and %g still drops them as usual.
    eval_eq(r#"printf('%!.30f', 0.1)"#, text(r#"0.1000000000000000055"#));
    eval_eq(r#"printf('%!.20f', 2.675)"#, text(r#"2.674999999999999823"#));
    eval_eq(r#"printf('%!.30e', 0.1)"#, text(r#"1.000000000000000055e-01"#));
    eval_eq(r#"printf('%!.30e', 1.0/3)"#, text(r#"3.333333333333333148e-01"#));
    eval_eq(r#"printf('%!.25e', -2.675)"#, text(r#"-2.674999999999999823e+00"#));
    eval_eq(r#"printf('%!.20g', 0.1)"#, text(r#"0.1000000000000000055"#));
    eval_eq(r#"printf('%!.20g', 2.675)"#, text(r#"2.674999999999999823"#));
    eval_eq(r#"printf('%!.17g', 49.47)"#, text(r#"49.469999999999999"#));
    eval_eq(r#"printf('%!.30f', 0.5)"#, text(r#"0.5"#));
    eval_eq(r#"printf('%!.0f', 0.5)"#, text(r#"1.0"#));
    eval_eq(r#"printf('%!f', 3.5)"#, text(r#"3.5"#));
    eval_eq(r#"printf('%!e', 1.0)"#, text(r#"1.0e+00"#));
    eval_eq(r#"printf('%!.3f', 1.5)"#, text(r#"1.5"#));
    eval_eq(r#"printf('%!g', 2.0)"#, text(r#"2.0"#));
    eval_eq(r#"printf('%!.0e', 3.0)"#, text(r#"3.0e+00"#));
}

#[test]
fn printf_float_sign_and_flags() {
    // Signs, -0.0, width, zero padding, commas and '#' around the new rounding.
    eval_eq(r#"printf('%.0f', -0.4)"#, text(r#"-0"#));
    eval_eq(r#"printf('%#.0f', -0.4)"#, text(r#"0."#));
    eval_eq(r#"printf('%#.2f', -0.004)"#, text(r#"0.00"#));
    eval_eq(r#"printf('%#.2f', -0.006)"#, text(r#"-0.01"#));
    eval_eq(r#"printf('%+.0f', -0.4)"#, text(r#"-0"#));
    eval_eq(r#"printf('%.2f', -0.0)"#, text(r#"0.00"#));
    eval_eq(r#"printf('%g', -0.0)"#, text(r#"0"#));
    eval_eq(r#"printf('%e', -0.0)"#, text(r#"0.000000e+00"#));
    eval_eq(r#"printf('%+.0f', 0.5)"#, text(r#"+1"#));
    eval_eq(r#"printf('% .2e', 0.125)"#, text(r#" 1.25e-01"#));
    eval_eq(r#"printf('%010.2f', -0.125)"#, text(r#"-000000.13"#));
    eval_eq(r#"printf('%-10.1f|', 0.25)"#, text(r#"0.3       |"#));
    eval_eq(r#"printf('%,.2f', 1234567.125)"#, text(r#"1,234,567.13"#));
    eval_eq(r#"printf('%,.0f', 1234567.5)"#, text(r#"1,234,568"#));
    eval_eq(r#"printf('%,.2f', -1e20)"#, text(r#"-100,000,000,000,000,000,000.00"#));
    eval_eq(r#"printf('%#.3g', 1.0)"#, text(r#"1.00"#));
    eval_eq(r#"printf('%#.0e', 2.5)"#, text(r#"3.e+00"#));
    eval_eq(r#"printf('%#g', 0.125)"#, text(r#"0.125000"#));
    eval_eq(r#"printf('%.3G', 1e-10)"#, text(r#"1E-10"#));
    eval_eq(r#"printf('%E', 0.5)"#, text(r#"5.000000E-01"#));
    eval_eq(r#"printf('%020.3e', -1.5)"#, text(r#"-00000000001.500e+00"#));
    eval_eq(r#"printf('%05e', 1e999)"#, text(r#"9.000000e+999"#));
    eval_eq(r#"printf('%05g', -1e999)"#, text(r#"-9e+999"#));
    eval_eq(r#"printf('%10f|', 1e999)"#, text(r#"       Inf|"#));
    eval_eq(r#"printf('%*.*f', 10, 2, 0.125)"#, text(r#"      0.13"#));
}
