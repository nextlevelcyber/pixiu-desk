use super::*;

fn fx(s: &str) -> Fx {
    Fx::parse(s).expect("合法的十进制数")
}

#[test]
fn parse_and_print_round_trip() {
    for s in ["0", "1", "-1", "80715.4", "0.0001", "-0.05", "123.456000"] {
        assert_eq!(fx(s).to_string(), s);
    }
    assert_eq!(fx("123.456000").to_plain(), "123.456");
    assert_eq!(fx("-0.000").to_plain(), "0");
    assert_eq!(fx(".5").to_string(), "0.5");
    assert!(Fx::parse("1e5").is_err());
    assert!(Fx::parse("").is_err());
    assert!(Fx::parse("abc").is_err());
}

#[test]
fn arithmetic_is_exact() {
    assert_eq!(fx("0.1").plus(fx("0.2")).expect("加").to_plain(), "0.3");
    assert_eq!(
        fx("80715.4").minus(fx("80715.5")).expect("减").to_plain(),
        "-0.1"
    );
    assert_eq!(
        fx("0.0002").times(fx("80605.4")).expect("乘").to_plain(),
        "16.12108"
    );
    assert_eq!(Fx::new(8060540000000, 8).to_plain(), "80605.4");
}

#[test]
fn division_rounds_half_even() {
    // 与 Python Decimal.quantize(ROUND_HALF_EVEN) 一致。
    assert_eq!(fx("1").div_int(8, 2).expect("除").to_string(), "0.12");
    assert_eq!(fx("3").div_int(8, 2).expect("除").to_string(), "0.38");
    assert_eq!(fx("0.125").round_to(2).expect("舍入").to_string(), "0.12");
    assert_eq!(fx("0.135").round_to(2).expect("舍入").to_string(), "0.14");
    assert_eq!(fx("-0.125").round_to(2).expect("舍入").to_string(), "-0.12");
    assert_eq!(fx("-0.135").round_to(2).expect("舍入").to_string(), "-0.14");
    assert_eq!(fx("-2").div_int(3, 3).expect("除").to_string(), "-0.667");
    assert!(fx("1").div(Fx::ZERO, 2).is_err());
}

#[test]
fn fixed_formats_like_live_report() {
    assert_eq!(fx("1.5").fixed(3).expect("fixed"), "1.500");
    assert_eq!(
        fx("-0.0004").fixed_unsigned_zero(3).expect("fixed"),
        "0.000"
    );
    assert_eq!(
        fx("-0.0006").fixed_unsigned_zero(3).expect("fixed"),
        "-0.001"
    );
    assert_eq!(fx("12").fixed(2).expect("fixed"), "12.00");
}

#[test]
fn ordering_across_scales() {
    assert!(fx("1.10") == fx("1.1"));
    assert!(fx("-0.5") < fx("0"));
    assert!(fx("2") > fx("1.999999"));
}
