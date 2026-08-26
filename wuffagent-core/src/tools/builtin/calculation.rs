use std::collections::HashMap;

use crate::tools::types::{Tool, ToolOutput, ToolParams, ToolSchema};

/// A tool that evaluates mathematical expressions.
pub struct CalculationTool;

impl CalculationTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CalculationTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for CalculationTool {
    fn name(&self) -> &str {
        "calculation"
    }

    fn description(&self) -> &str {
        "Evaluate mathematical expressions. Supports basic arithmetic (+, -, *, /), exponentiation (^), parentheses, negative numbers, common functions (sin, cos, tan, asin, acos, atan, sqrt, log, ln, log2, log10, abs, floor, ceil, exp, round, fact), and constants (pi, e)."
    }

    fn parameters_schema(&self) -> ToolSchema {
        ToolSchema {
            name: "calculation".to_string(),
            description: "Evaluate a mathematical expression".to_string(),
            input_type: Some(crate::tools::types::JsonSchema {
                type_name: "object".to_string(),
                properties: Some({
                    let mut map = HashMap::new();
                    map.insert(
                        "expression".to_string(),
                        crate::tools::types::FieldSchema {
                            type_name: "string".to_string(),
                            description: "Mathematical expression to evaluate. Examples: \"2 + 3\", \"(10 * 5) / 2\", \"sin(pi / 2)\", \"sqrt(144)\", \"2 ^ 10\"".to_string(),
                            nullable: false,
                        },
                    );
                    map
                }),
                required: vec!["expression".to_string()],
            }),
        }
    }

    fn execute(&self, params: ToolParams) -> crate::tools::types::ToolResult<ToolOutput> {
        let expression: String = params
            .get("expression")
            .ok_or_else(|| {
                crate::tools::types::ToolError::InvalidParams("expression is required".to_string())
            })?;

        // Detect shell commands and guide the agent to use the `shell` tool instead.
        if looks_like_shell_command(&expression) {
            return Err(crate::tools::types::ToolError::Execution(
                "This appears to be a shell command, not a mathematical expression. Use the 'shell' tool to execute shell commands.".to_string()
            ));
        }

        let result = evaluate_expression(&expression).map_err(|e| {
            crate::tools::types::ToolError::Execution(format!("Evaluation error: {}", e))
        })?;

        Ok(ToolOutput::Success(serde_json::json!({
            "expression": expression,
            "result": result
        })))
    }
}

/// Detect if the string looks like a shell command rather than a math expression.
fn looks_like_shell_command(input: &str) -> bool {
    let trimmed = input.trim();
    let shell_indicators = [
        "cargo ", "npm ", "git ", "python ", "node ", "docker ", "make ", "cmake ",
        "rustc ", "clang ", "gcc ", "g++ ", "rust-analyzer ",
        "echo ", "ls ", "cd ", "mkdir ", "rm ", "cp ", "mv ", "cat ",
        "curl ", "wget ", "pip ", "conda ", "brew ", "apt ", "yum ",
        "--manifest-path", "2>&1", "&&", ";", "|", ">",
    ];
    shell_indicators.iter().any(|&ind| trimmed.contains(ind))
}

/// Evaluate a mathematical expression supporting:
/// - Basic arithmetic: +, -, *, /, ^ (exponentiation)
/// - Parentheses for grouping
/// - Negative numbers (unary minus)
/// - Common functions: sin, cos, tan, asin, acos, atan, sqrt, log, ln, log2, log10, abs, floor, ceil, exp, round, fact
/// - Constants: pi, e
/// - Scientific notation (e.g. 1.5e10)
fn evaluate_expression(expr: &str) -> Result<f64, String> {
    let tokens: Vec<Token> = tokenize(expr).map_err(|e| e.to_string())?;
    let mut parser = Parser { tokens, pos: 0 };
    let result = parser.parse_expression();
    if parser.pos != parser.tokens.len() {
        return Err("Unexpected tokens after expression".to_string());
    }
    result
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Func(String),       // function name (sin, cos, etc.)
    Const(String),      // constant name (pi, e)
    Plus,
    Minus,
    Mul,
    Div,
    Pow,                // ^ operator
    LParen,
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\n' | '\r' => { i += 1; continue; }
            '+' => { tokens.push(Token::Plus); i += 1; }
            '-' => { tokens.push(Token::Minus); i += 1; }
            '*' => { tokens.push(Token::Mul); i += 1; }
            '/' => { tokens.push(Token::Div); i += 1; }
            '^' => { tokens.push(Token::Pow); i += 1; }
            '(' => { tokens.push(Token::LParen); i += 1; }
            ')' => { tokens.push(Token::RParen); i += 1; }
            c if c.is_ascii_digit() || (c == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) => {
                let mut num_str = String::new();
                // Handle scientific notation (e.g. 1.5e10)
                num_str.push(c);
                while i + 1 < chars.len() && (chars[i + 1].is_ascii_digit() || chars[i + 1] == '.') {
                    i += 1;
                    num_str.push(chars[i]);
                }
                if i + 1 < chars.len() && (chars[i + 1].to_lowercase().next() == Some('e')) {
                    // Check for scientific notation
                    i += 1;
                    num_str.push(chars[i]); // 'e'
                    if i + 1 < chars.len() && (chars[i + 1] == '+' || chars[i + 1] == '-') {
                        i += 1;
                        num_str.push(chars[i]);
                    }
                    while i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                        i += 1;
                        num_str.push(chars[i]);
                    }
                }
                let n: f64 = num_str.parse()
                    .map_err(|_| format!("Invalid number: {}", num_str))?;
                tokens.push(Token::Number(n));
                i += 1; // advance past the last digit (inner loops may not have moved i)
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut name = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    name.push(chars[i]);
                    i += 1;
                }
                let lower = name.to_lowercase();
                match lower.as_str() {
                    "pi" => tokens.push(Token::Const("pi".to_string())),
                    "e" => tokens.push(Token::Const("e".to_string())),
                    _ => tokens.push(Token::Func(lower)),
                }
            }
            _ => return Err(format!("Unexpected character: {} at position {}", chars[i], i)),
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let t = self.tokens.get(self.pos);
        self.pos += 1;
        t
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        match self.advance() {
            Some(t) if t == expected => Ok(()),
            Some(t) => Err(format!("Expected {:?}, got {:?}", expected, t)),
            None => Err(format!("Expected {:?}, but reached end of expression", expected)),
        }
    }

    fn parse_expression(&mut self) -> Result<f64, String> {
        let mut left = self.parse_term()?;
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.advance();
                    let right = self.parse_term()?;
                    left += right;
                }
                Some(Token::Minus) => {
                    self.advance();
                    let right = self.parse_term()?;
                    left -= right;
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_term(&mut self) -> Result<f64, String> {
        let mut left = self.parse_power()?;
        loop {
            match self.peek() {
                Some(Token::Mul) => {
                    self.advance();
                    let right = self.parse_power()?;
                    left *= right;
                }
                Some(Token::Div) => {
                    self.advance();
                    let right = self.parse_power()?;
                    if right == 0.0 {
                        return Err("Division by zero".to_string());
                    }
                    left /= right;
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_power(&mut self) -> Result<f64, String> {
        let base = self.parse_factor()?;
        if let Some(Token::Pow) = self.peek() {
            self.advance();
            // Right-associative: 2^3^2 = 2^(3^2) = 512
            let exponent = self.parse_power()?;
            return Ok(base.powf(exponent));
        }
        Ok(base)
    }

    fn parse_factor(&mut self) -> Result<f64, String> {
        let token = self.peek().cloned().ok_or("Unexpected end of expression")?;
        match token {
            Token::Number(n) => {
                self.advance();
                Ok(n)
            }
            Token::Const(name) => {
                self.advance();
                Ok(match name.as_str() {
                    "pi" => std::f64::consts::PI,
                    "e" => std::f64::consts::E,
                    _ => return Err(format!("Unknown constant: {}", name)),
                })
            }
            Token::Minus => {
                self.advance();
                // Bind looser than ^: -2^2 = -(2^2) = -4
                let factor = self.parse_power()?;
                Ok(-factor)
            }
            Token::Func(name) => {
                let func_name = name.clone();
                self.advance();
                self.expect(&Token::LParen)?;
                let arg = self.parse_expression()?;
                self.expect(&Token::RParen)?;
                Ok(Self::call_function(&func_name, arg)?)
            }
            Token::LParen => {
                self.advance();
                let result = self.parse_expression()?;
                if let Some(Token::RParen) = self.peek() {
                    self.advance();
                } else {
                    return Err("Missing closing parenthesis".to_string());
                }
                Ok(result)
            }
            _ => Err("Expected number, constant, function, or expression".to_string()),
        }
    }

    fn call_function(name: &str, arg: f64) -> Result<f64, String> {
        match name {
            "sin" => Ok(arg.sin()),
            "cos" => Ok(arg.cos()),
            "tan" => Ok(arg.tan()),
            "asin" | "acos" => {
                if arg < -1.0 || arg > 1.0 {
                    return Err(format!("{} argument out of range: must be in [-1, 1], got {}", name, arg));
                }
                if name == "asin" {
                    Ok(arg.asin())
                } else {
                    Ok(arg.acos())
                }
            }
            "atan" => Ok(arg.atan()),
            "sqrt" => {
                if arg < 0.0 {
                    return Err("Square root of negative number".to_string());
                }
                Ok(arg.sqrt())
            }
            "log" | "ln" => {
                if arg <= 0.0 {
                    return Err("Natural log of non-positive number".to_string());
                }
                Ok(arg.ln())
            }
            "log2" => {
                if arg <= 0.0 {
                    return Err("Log2 of non-positive number".to_string());
                }
                Ok(arg.log2())
            }
            "log10" => {
                if arg <= 0.0 {
                    return Err("Log10 of non-positive number".to_string());
                }
                Ok(arg.log10())
            }
            "abs" => Ok(arg.abs()),
            "floor" => Ok(arg.floor()),
            "ceil" => Ok(arg.ceil()),
            "exp" => Ok(arg.exp()),
            "round" => Ok(arg.round()),
            "fact" => {
                if arg < 0.0 || arg != arg.floor() {
                    return Err("Factorial requires a non-negative integer".to_string());
                }
                let n = arg as u64;
                if n > 170 {
                    return Err("Factorial argument too large (max 170)".to_string());
                }
                let mut result = 1u64;
                for i in 1..=n {
                    result *= i;
                }
                Ok(result as f64)
            }
            _ => Err(format!("Unknown function: {}", name)),
        }
    }
}

#[cfg(test)]
mod tests {
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
        assert!((evaluate_expression("sqrt(2 ^ 10 + 14 ^ 2)").unwrap() - 1220.0f64.sqrt()).abs() < 1e-9);
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
        assert!(looks_like_shell_command("cargo check --manifest-path M:/repos/WuffAgent/Cargo.toml 2>&1"));
        assert!(looks_like_shell_command("git status"));
        assert!(looks_like_shell_command("npm run build"));
        assert!(!looks_like_shell_command("2 + 2"));
        assert!(!looks_like_shell_command("(10 * 5) / 2"));
        assert!(!looks_like_shell_command("sin(pi / 2)"));
        assert!(!looks_like_shell_command("sqrt(144)"));
    }
}