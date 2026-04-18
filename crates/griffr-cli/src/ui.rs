use chrono::{DateTime, Utc};
use serde_json::Value;

pub fn print_phase(message: impl AsRef<str>) {
    println!("==> {}", message.as_ref());
}

pub fn print_success(message: impl AsRef<str>) {
    println!("OK: {}", message.as_ref());
}

pub fn print_info(message: impl AsRef<str>) {
    println!("{}", message.as_ref());
}

pub fn print_warning(message: impl AsRef<str>) {
    eprintln!("warning: {}", message.as_ref());
}

pub fn print_kv_section(title: &str, rows: &[(String, String)]) {
    if rows.is_empty() {
        return;
    }

    print!("{}", render_kv_section(title, rows));
}

pub fn render_kv_section(title: &str, rows: &[(String, String)]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut out = String::new();
    out.push_str(title);
    out.push('\n');
    for (key, value) in rows {
        out.push_str(&format!("  {:width$} : {}\n", key, value, width = width));
    }
    out
}

pub fn emit_json(value: &Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GiB", b / GB)
    } else if b >= MB {
        format!("{:.2} MiB", b / MB)
    } else if b >= KB {
        format!("{:.2} KiB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

pub fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;

    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn format_unix_ms(ts_ms: &str) -> Option<String> {
    let millis = ts_ms.parse::<i64>().ok()?;
    let dt = DateTime::<Utc>::from_timestamp_millis(millis)?;
    Some(dt.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_humanized() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MiB");
    }

    #[test]
    fn html_tags_are_removed() {
        assert_eq!(
            strip_html_tags("<p>Hello <b>Doctor</b></p>"),
            "Hello Doctor"
        );
    }

    #[test]
    fn unix_ms_is_formatted() {
        assert_eq!(
            format_unix_ms("0").as_deref(),
            Some("1970-01-01T00:00:00+00:00")
        );
        assert!(format_unix_ms("bad-value").is_none());
    }

    #[test]
    fn kv_section_render_is_stable() {
        let output = render_kv_section(
            "Remote State",
            &[
                ("version".to_string(), "1.1.9".to_string()),
                ("server".to_string(), "cn_official".to_string()),
            ],
        );
        assert_eq!(
            output,
            "Remote State\n  version : 1.1.9\n  server  : cn_official\n"
        );
    }
}
