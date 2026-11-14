//! Deciding whether a clip is a table, and in what form.

use std::path::{Path, PathBuf};

/// What kind of table text is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextKind {
    /// Delimited text; the delimiter that was found.
    Delimited(char),
    /// A JSON array of objects, or one object per line.
    Json,
    /// A Markdown table (`| a | b |`).
    Markdown,
}

const SAMPLE_LINES: usize = 30;

fn split_markdown_row(line: &str) -> Option<Vec<String>> {
    let t = line.trim();
    if !t.starts_with('|') || !t.ends_with('|') || t.len() < 3 {
        return None;
    }
    Some(t[1..t.len() - 1].split('|').map(|c| c.trim().to_string()).collect())
}

fn is_markdown_rule(cells: &[String]) -> bool {
    !cells.is_empty() && cells.iter().all(|c| c.len() >= 3 && c.chars().all(|ch| ch == '-' || ch == ':'))
}

/// Count `delimiter` outside double quotes.
fn count_outside_quotes(line: &str, delimiter: char) -> usize {
    let mut quoted = false;
    let mut n = 0;
    for c in line.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if c == delimiter && !quoted {
            n += 1;
        }
    }
    n
}

/// Is this text a table? Prose with commas in it is not: a table has the
/// same number of delimiters on every line.
pub fn sniff_text(text: &str) -> Option<TextKind> {
    let trimmed = text.trim();
    if trimmed.len() < 3 {
        return None;
    }
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        let rows = match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(serde_json::Value::Array(items)) => items,
            // One object per line.
            _ => trimmed.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap_or(serde_json::Value::Null)).collect(),
        };
        let objects = !rows.is_empty() && rows.iter().all(|r| r.as_object().is_some_and(|o| !o.is_empty()));
        return objects.then_some(TextKind::Json);
    }
    let lines: Vec<&str> = trimmed.lines().filter(|l| !l.trim().is_empty()).take(SAMPLE_LINES).collect();
    if lines.len() < 2 {
        return None;
    }
    if let (Some(_), Some(rule)) = (split_markdown_row(lines[0]), split_markdown_row(lines[1])) {
        if is_markdown_rule(&rule) {
            return Some(TextKind::Markdown);
        }
    }
    for delimiter in ['\t', ',', ';', '|'] {
        let counts: Vec<usize> = lines.iter().map(|l| count_outside_quotes(l, delimiter)).collect();
        let first = counts[0];
        if first == 0 || counts.iter().any(|c| *c != first) {
            continue;
        }
        // Commas agree by chance in short prose ("Yes, please.\nNo, thanks.").
        // Ask for more evidence there: three columns, or three lines.
        if delimiter == ',' && first < 2 && lines.len() < 3 {
            continue;
        }
        return Some(TextKind::Delimited(delimiter));
    }
    None
}

/// A Markdown table as tab-separated text.
pub fn markdown_to_tsv(text: &str) -> String {
    text.lines()
        .filter_map(split_markdown_row)
        .filter(|cells| !is_markdown_rule(cells))
        .map(|cells| cells.iter().map(|c| c.replace('\t', " ")).collect::<Vec<_>>().join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The kind of data file, by extension.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FileKind {
    Csv,
    Json,
    Parquet,
}

pub fn file_kind(path: &Path) -> Option<FileKind> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "csv" | "tsv" | "tab" | "psv" => Some(FileKind::Csv),
        "json" | "jsonl" | "ndjson" => Some(FileKind::Json),
        "parquet" | "pq" => Some(FileKind::Parquet),
        _ => None,
    }
}

/// Copied files that can be queried together: all data files of one kind.
pub fn data_files(paths: &[PathBuf]) -> Option<(FileKind, Vec<PathBuf>)> {
    let first = file_kind(paths.first()?)?;
    paths.iter().all(|p| file_kind(p) == Some(first) && p.is_file()).then(|| (first, paths.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_in_text() {
        assert_eq!(sniff_text("name,qty,price\nwidget,2,3.50\ngadget,1,12.00"), Some(TextKind::Delimited(',')));
        assert_eq!(sniff_text("name\tqty\nwidget\t2"), Some(TextKind::Delimited('\t')));
        assert_eq!(sniff_text("a;b;c\n1;2;3"), Some(TextKind::Delimited(';')));
        assert_eq!(sniff_text("city,note\n\"Leeds, UK\",ok\n\"Lyon, FR\",ok"), Some(TextKind::Delimited(',')));
        assert_eq!(sniff_text("| a | b |\n|---|---|\n| 1 | 2 |"), Some(TextKind::Markdown));
        assert_eq!(sniff_text(r#"[{"a":1,"b":"x"},{"a":2,"b":"y"}]"#), Some(TextKind::Json));
        assert_eq!(sniff_text("{\"a\":1}\n{\"a\":2}"), Some(TextKind::Json));
    }

    #[test]
    fn things_that_are_not_tables() {
        for text in [
            "Just a sentence.",
            "Yes, please.\nNo, thanks.",
            "Dear Sam,\nthe report, as promised, is attached.\nBest,\nJo",
            "fn main() {\n    println!(\"a, b\");\n}",
            "[1, 2, 3]",
            "{\"not\": \"rows\", \"nested\": {}}\nand then prose",
            "one line, with, commas",
            "",
        ] {
            assert_eq!(sniff_text(text), None, "{text:?}");
        }
    }

    #[test]
    fn markdown_tables_become_tsv() {
        assert_eq!(markdown_to_tsv("| name | qty |\n|:--|--:|\n| widget | 2 |\n| gadget | 1 |"), "name\tqty\nwidget\t2\ngadget\t1");
    }

    #[test]
    fn data_files_must_agree() {
        assert_eq!(file_kind(Path::new("/x/Sales.CSV")), Some(FileKind::Csv));
        assert_eq!(file_kind(Path::new("/x/rows.parquet")), Some(FileKind::Parquet));
        assert_eq!(file_kind(Path::new("/x/notes.txt")), None);
        assert!(data_files(&[]).is_none());
    }
}
