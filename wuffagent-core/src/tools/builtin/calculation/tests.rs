//! Unit tests for the `calculation` module (see `super`).

use super::*;

#[test]
fn test_evaluate_basic() {
    assert!((evaluate_expression("2 + 3").unwrap() - 5.0).abs() < 1e-9);
    assert!((evaluate_expression("10 - 4").unwrap() - 6.0).abs() < 1e-9);
    assert!((evaluate_expression("3 * 4").unwrap() - 12.0).abs() < 1e-9);
    assert!((evaluate_expression("15 / 3").unwrap() - 5.0).abs() < 1e-9);
    assert!((evaluate_expression("(2 + 3) * 4").unwrap() - 20.0).abs() < 1e-9);
}

#[test]
fn test_evaluate_exponentiation() {
    assert!((evaluate_expression("2 ^ 10").unwrap() - 1024.0).abs() < 1e-9);
    assert!((evaluate_expression("3 ^ 3").unwrap() - 27.0).abs() < 1e-9);
    assert!((evaluate_expression("2 ^ 3 ^ 2").unwrap() - 512.0).abs() < 1e-9); // right-associative: 2^(3^2) = 2^9 = 512
    assert!((evaluate_expression("-2 ^ 2").unwrap() + 4.0).abs() < 1e-9); // unary minus binds looser than ^
    assert!((evaluate_expression("2 * -3").unwrap() + 6.0).abs() < 1e-9);
    assert!((evaluate_expression("-sin(pi / 2)").unwrap() + 1.0).abs() < 1e-9);
}

#[test]
fn test_evaluate_functions() {
    assert!((evaluate_expression("sin(0)").unwrap() - 0.0).abs() < 1e-9);
    assert!((evaluate_expression("sin(pi / 2)").unwrap() - 1.0).abs() < 1e-9);
    assert!((evaluate_expression("cos(0)").unwrap() - 1.0).abs() < 1e-9);
    assert!((evaluate_expression("sqrt(144)").unwrap() - 12.0).abs() < 1e-9);
    assert!((evaluate_expression("log(e)").unwrap() - 1.0).abs() < 1e-9);
    assert!((evaluate_expression("abs(-5)").unwrap() - 5.0).abs() < 1e-9);
    assert!((evaluate_expression("floor(3.7)").unwrap() - 3.0).abs() < 1e-9);
    assert!((evaluate_expression("ceil(3.2)").unwrap() - 4.0).abs() < 1e-9);
    assert!((evaluate_expression("exp(0)").unwrap() - 1.0).abs() < 1e-9);
    assert!((evaluate_expression("fact(5)").unwrap() - 120.0).abs() < 1e-9);
    assert!((evaluate_expression("round(3.6)").unwrap() - 4.0).abs() < 1e-9);
    assert!((evaluate_expression("log2(8)").unwrap() - 3.0).abs() < 1e-9);
}

#[test]
fn test_evaluate_constants() {
    let pi_result = evaluate_expression("pi").unwrap();
    assert!((pi_result - std::f64::consts::PI).abs() < 1e-9);
    let e_result = evaluate_expression("e").unwrap();
    assert!((e_result - std::f64::consts::E).abs() < 1e-9);
}

#[test]
fn test_evaluate_scientific_notation() {
    assert!((evaluate_expression("1.5e10").unwrap() - 15000000000.0).abs() < 1e-9);
    assert!((evaluate_expression("1e-3").unwrap() - 0.001).abs() < 1e-9);
}

#[test]
fn test_evaluate_complex() {
    assert!((evaluate_expression("2 * pi").unwrap() - 2.0 * std::f64::consts::PI).abs() < 1e-9);
    // sqrt(1024 + 196) = sqrt(1220) ≈ 34.9285
    assert!(
        (evaluate_expression("sqrt(2 ^ 10 + 14 ^ 2)").unwrap() - 1220.0f64.sqrt()).abs() < 1e-9
    );
    assert!((evaluate_expression("sqrt(12 ^ 2 + 5 ^ 2)").unwrap() - 13.0).abs() < 1e-9);
    assert!((evaluate_expression("sin(pi) / 2 + 1").unwrap() - 1.0).abs() < 1e-9);
}

#[test]
fn test_evaluate_error() {
    assert!(evaluate_expression("2 +").is_err());
    assert!(evaluate_expression("10 / 0").is_err());
    assert!(evaluate_expression("sqrt(-1)").is_err());
    assert!(evaluate_expression("fact(3.5)").is_err());
    assert!(evaluate_expression("asin(2)").is_err());
    assert!(evaluate_expression("acos(-2)").is_err());
}

#[test]
fn test_shell_command_detection() {
    assert!(looks_like_shell_command(
        "cargo check --manifest-path M:/repos/WuffAgent/Cargo.toml 2>&1"
    ));
    assert!(looks_like_shell_command("git status"));
    assert!(looks_like_shell_command("npm run build"));
    assert!(!looks_like_shell_command("2 + 2"));
    assert!(!looks_like_shell_command("(10 * 5) / 2"));
    assert!(!looks_like_shell_command("sin(pi / 2)"));
    assert!(!looks_like_shell_command("sqrt(144)"));
}
