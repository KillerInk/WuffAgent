/// Parses a progress percentage from a server log line.
///
/// llama-server outputs lines like: "loading model ... 100%"
/// This function extracts the numeric percentage before the `%` character.
pub fn parse_progress(line: &str) -> Option<f32> {
    // llama-server outputs lines like: "loading model ... 100%"
    if let Some(pos) = line.find('%') {
        let before = &line[..pos];
        if let Some(last_space) = before.rfind(' ') {
            if let Ok(pct) = before[last_space + 1..].parse::<f32>() {
                return Some(pct);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_progress() {
        assert_eq!(parse_progress("loading model ... 100%"), Some(100.0));
        assert_eq!(parse_progress("loading model ... 50%"), Some(50.0));
        assert_eq!(parse_progress("loading model ... 75.5%"), Some(75.5));
        assert_eq!(parse_progress("no percentage here"), None);
        assert_eq!(parse_progress(""), None);
        assert_eq!(parse_progress("100"), None);
    }
}
