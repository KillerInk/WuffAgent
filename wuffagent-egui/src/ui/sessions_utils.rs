/// Format a chrono DateTime<Utc> to a human-readable relative time string
/// like "just now", "5 min ago", "2 hours ago", "3 days ago".
pub fn relative_time(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(*dt);
    let seconds = duration.num_seconds().abs();

    if seconds < 10 {
        "just now".to_string()
    } else if seconds < 60 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 3600 {
        let minutes = seconds / 60;
        if minutes < 2 {
            "1 min ago".to_string()
        } else {
            format!("{} min ago", minutes)
        }
    } else if seconds < 86400 {
        let hours = seconds / 3600;
        if hours < 2 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else if seconds < 172800 {
        "yesterday".to_string()
    } else {
        let days = seconds / 86400;
        if days < 30 {
            format!("{} days ago", days)
        } else if days < 365 {
            let months = days / 30;
            if months < 2 {
                "1 month ago".to_string()
            } else {
                format!("{} months ago", months)
            }
        } else {
            let years = days / 365;
            if years < 2 {
                "1 year ago".to_string()
            } else {
                format!("{} years ago", years)
            }
        }
    }
}

/// Truncate a string to at most `max_chars` characters, adding "..." if truncated.
pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(max_chars).collect::<String>())
    }
}
