use chrono::{DateTime, Utc};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Release);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Acquire)
}

pub fn print_phase(message: impl AsRef<str>) {
    if !is_quiet() {
        println!("==> {}", message.as_ref());
    }
}

pub fn print_success(message: impl AsRef<str>) {
    if !is_quiet() {
        println!("OK: {}", message.as_ref());
    }
}

pub fn print_info(message: impl AsRef<str>) {
    if !is_quiet() {
        println!("{}", message.as_ref());
    }
}

pub fn print_warning(message: impl AsRef<str>) {
    eprintln!("warning: {}", message.as_ref());
}

pub fn print_kv_section(title: &str, rows: &[(String, String)]) {
    if is_quiet() || rows.is_empty() {
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

pub fn emit_json(value: &impl Serialize) -> anyhow::Result<()> {
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

pub fn print_patch_check(report: &griffr_runtime::PatchCheckReport) {
    let available_install = report
        .available_install_bytes
        .map(format_bytes)
        .unwrap_or_else(|| "unknown".to_string());
    let available_vfs = report
        .available_vfs_bytes
        .map(format_bytes)
        .unwrap_or_else(|| "unknown".to_string());
    let available_work = report
        .available_work_bytes
        .map(format_bytes)
        .unwrap_or_else(|| "unknown".to_string());
    print_kv_section(
        "Patch Check",
        &[
            (
                "archive expanded".to_string(),
                format_bytes(report.archive_uncompressed_bytes),
            ),
            (
                "planned extract".to_string(),
                format_bytes(report.planned_extract_bytes),
            ),
            (
                "final growth".to_string(),
                format_bytes(report.estimated_final_growth_bytes),
            ),
            (
                "install peak".to_string(),
                format_bytes(report.estimated_install_peak_bytes),
            ),
            (
                "VFS peak".to_string(),
                format_bytes(report.estimated_vfs_peak_bytes),
            ),
            (
                "work space".to_string(),
                format_bytes(report.estimated_work_bytes),
            ),
            ("install available".to_string(), available_install),
            ("VFS available".to_string(), available_vfs),
            ("work available".to_string(), available_work),
            (
                "manifest work".to_string(),
                format!(
                    "{} patch entries, {} delete entries",
                    report.patch_entries, report.delete_entries
                ),
            ),
        ],
    );
}

pub fn strip_html_tags(input: &str) -> String {
    const BLOCK_TAGS: &[&str] = &[
        "address",
        "article",
        "aside",
        "blockquote",
        "br",
        "div",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "li",
        "main",
        "nav",
        "ol",
        "p",
        "pre",
        "section",
        "table",
        "tr",
        "ul",
    ];

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }

        let mut tag = String::new();
        for tag_ch in chars.by_ref() {
            if tag_ch == '>' {
                break;
            }
            tag.push(tag_ch);
        }
        let name = tag
            .trim_start()
            .trim_start_matches('/')
            .split(|ch: char| ch.is_ascii_whitespace() || ch == '/' || ch == '>')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if BLOCK_TAGS.contains(&name.as_str()) && !out.ends_with(char::is_whitespace) {
            out.push(' ');
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
                ("channel".to_string(), "1".to_string()),
            ],
        );
        assert_eq!(output, "Remote State\n  version : 1.1.9\n  channel : 1\n");
    }
}
