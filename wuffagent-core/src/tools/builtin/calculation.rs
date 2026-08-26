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
        "Perform mathematical calculations from an expression string"
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
                            description: "Mathematical expression to evaluate".to_string(),
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

        // Use the `evalu8` crate for safe expression evaluation.
        // For now we use a simple parser; replace with a proper library in production.
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
    // Common shell command indicators
    let shell_indicators = [
        "cargo ", "npm ", "git ", "python ", "node ", "docker ", "make ", "cmake ",
        "rustc ", "clang ", "gcc ", "g++ ", "rust-analyzer ",
        "echo ", "ls ", "cd ", "mkdir ", "rm ", "cp ", "mv ", "cat ",
        "curl ", "wget ", "pip ", "conda ", "brew ", "apt ", "yum ",
        "--manifest-path", "2>&1", "&&", ";", "|", ">",
    ];
    shell_indicators.iter().any(|&ind| trimmed.contains(ind))
}

/// Simple expression evaluator supporting +, -, *, /, parentheses.
fn evaluate_expression(expr: &str) -> Result<f64, String> {
    // Tokenize
    let tokens: Vec<Token> = tokenize(expr).map_err(|e| e.to_string())?;
    let mut parser = Parser { tokens, pos: 0 };
    let result = parser.parse_expression();
    if parser.pos != parser.tokens.len() {
        return Err("Unexpected tokens after expression".to_string());
    }
    result
}

#[derive(Debug, Clone)]
enum Token {
    Number(f64),
    Plus,
    Minus,
    Mul,
    Div,
    LParen,
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' | '\r' => continue,
            '+' => tokens.push(Token::Plus),
            '-' => tokens.push(Token::Minus),
            '*' => tokens.push(Token::Mul),
            '/' => tokens.push(Token::Div),
            '(' => tokens.push(Token::LParen),
            ')' => tokens.push(Token::RParen),
            c if c.is_ascii_digit() || c == '.' => {
                let mut num_str = String::new();
                num_str.push(c);
                while let Some(&nc) = chars.peek() {
                    if nc.is_ascii_digit() || nc == '.' {
                        num_str.push(nc);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let n: f64 = num_str
                    .parse()
                    .map_err(|_| format!("Invalid number: {}", num_str))?;
                tokens.push(Token::Number(n));
            }
            _ => return Err(format!("Unexpected character: {}", c)),
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<Token> {
        self.tokens.get(self.pos).cloned()
    }

    fn advance(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
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
        let mut left = self.parse_factor()?;
        loop {
            match self.peek() {
                Some(Token::Mul) => {
                    self.advance();
                    let right = self.parse_factor()?;
                    left *= right;
                }
                Some(Token::Div) => {
                    self.advance();
                    let right = self.parse_factor()?;
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

    fn parse_factor(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(Token::Number(n)) => {
                self.advance();
                Ok(n)
            }
            Some(Token::LParen) => {
                self.advance();
                let result = self.parse_expression()?;
                if let Some(Token::RParen) = self.peek() {
                    self.advance();
                } else {
                    return Err("Missing closing parenthesis".to_string());
                }
                Ok(result)
            }
            Some(Token::Minus) => {
                self.advance();
                let factor = self.parse_factor()?;
                Ok(-factor)
            }
            _ => Err("Expected number or expression".to_string()),
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
    fn test_evaluate_error() {
        assert!(evaluate_expression("2 +").is_err());
        assert!(evaluate_expression("10 / 0").is_err());
    }

    #[test]
    fn test_shell_command_detection() {
        assert!(looks_like_shell_command("cargo check --manifest-path M:/repos/WuffAgent/Cargo.toml 2>&1"));
        assert!(looks_like_shell_command("git status"));
        assert!(looks_like_shell_command("npm run build"));
        assert!(!looks_like_shell_command("2 + 2"));
        assert!(!looks_like_shell_command("(10 * 5) / 2"));
    }
}
