//! Schrödinger "Query" plugin: run SQL over a copied table, with DuckDB.
//!
//! A clip is a table when it is delimited text (from a spreadsheet, a CSV),
//! a Markdown table, JSON rows, or copied data files (CSV, JSON, Parquet;
//! several of them are read as one table). It is loaded as a table named
//! `clip`, and a query's result is what gets pasted.
//!
//! There are saved queries (row count, totals, averages, counts and totals
//! by group, ...), and whatever the user types in the picker after `=`.

use schrodinger_plugin_sdk::prelude::*;
use schrodinger_plugin_sdk::schrodinger_plugin;
use std::path::PathBuf;

pub mod engine;
pub mod render;
pub mod source;

use engine::{Column, Db, Source};

#[derive(Default)]
pub struct QueryPlugin;

const TEXT_FORMATS: &[&str] = &["public.utf8-plain-text", "NSStringPboardType", "text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];
const FILE_FORMATS: &[&str] = &["public.file-url", "NSFilenamesPboardType", "text/uri-list"];
/// More text than this is not sniffed; copy the file instead.
const MAX_TEXT_BYTES: usize = 20 * 1024 * 1024;
/// Data files beyond this are not opened just to see what to offer.
const MAX_ANALYZE_BYTES: u64 = 100 * 1024 * 1024;
/// A result with more rows than this is refused.
const MAX_ROWS: usize = 10_000;

const MODES: &[&str] = &["summary", "columns", "count", "totals", "averages", "group-count", "group-totals", "distinct", "sorted", "head", "combined", "sql"];

fn has_any(formats: &[String], names: &[&str]) -> bool {
    formats.iter().any(|f| names.iter().any(|n| f.eq_ignore_ascii_case(n)))
}

fn path_from_file_url(url: &str) -> Option<PathBuf> {
    let rest = url.trim().trim_end_matches('\0').strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok().filter(|p| p.starts_with('/')).map(PathBuf::from)
}

fn copied_paths(entry: &ClipEntry) -> Vec<PathBuf> {
    if has_any(&entry.meta.formats, &["NSFilenamesPboardType"]) {
        if let Ok(bytes) = entry.read_format("NSFilenamesPboardType") {
            if let Ok(plist::Value::Array(items)) = plist::Value::from_reader(std::io::Cursor::new(&bytes)) {
                let paths: Vec<PathBuf> = items.iter().filter_map(|v| v.as_string()).map(PathBuf::from).collect();
                if !paths.is_empty() {
                    return paths;
                }
            }
        }
    }
    if has_any(&entry.meta.formats, &["text/uri-list"]) {
        if let Ok(bytes) = entry.read_format("text/uri-list") {
            return String::from_utf8_lossy(&bytes).lines().filter(|l| !l.starts_with('#')).filter_map(path_from_file_url).collect();
        }
    }
    entry.read_format("public.file-url").ok().and_then(|b| path_from_file_url(&String::from_utf8_lossy(&b))).into_iter().collect()
}

/// The table in this clip, if it is one.
fn source(entry: &ClipEntry) -> Option<Source> {
    if has_any(&entry.meta.formats, FILE_FORMATS) {
        let (kind, paths) = source::data_files(&copied_paths(entry))?;
        return Some(Source::Files { kind, paths });
    }
    let name = TEXT_FORMATS.iter().find(|n| has_any(&entry.meta.formats, &[n]))?;
    let bytes = entry.read_format(name).ok()?;
    if bytes.len() > MAX_TEXT_BYTES {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let kind = source::sniff_text(&text)?;
    Some(Source::Text { kind, text })
}

fn file_count(source: &Source) -> usize {
    match source {
        Source::Files { paths, .. } => paths.len(),
        Source::Text { .. } => 1,
    }
}

/// Run one mode and return the result.
pub fn run(source: &Source, mode: &str, input: Option<&str>) -> Result<render::Table, String> {
    let db = Db::open(source)?;
    let sql = if mode == "sql" {
        let typed = input.map(str::trim).filter(|s| !s.is_empty()).ok_or("Type = and a query")?;
        engine::expand(typed)
    } else {
        let columns = db.columns()?;
        engine::saved_query(mode, &columns, file_count(source)).ok_or("This query has nothing to work on in this table")?
    };
    db.run(&sql, MAX_ROWS)
}

fn text_format() -> &'static str {
    if cfg!(target_os = "macos") {
        "public.utf8-plain-text"
    } else {
        "text/plain;charset=utf-8"
    }
}

fn html_format() -> &'static str {
    if cfg!(target_os = "macos") {
        "public.html"
    } else {
        "text/html"
    }
}

/// The result as it goes to the clipboard: text in the chosen form, and a
/// real table alongside it for apps that paste rich text.
pub fn outputs(table: &render::Table, form: &str) -> Vec<Output> {
    // A lone value (a count, a total) is pasted as just that.
    if let Some(value) = table.single_value() {
        return vec![Output::new(text_format()).data(value.as_bytes().to_vec())];
    }
    let text = match form {
        f if f.starts_with("CSV") => render::delimited(table, ','),
        f if f.starts_with("Tab") => render::delimited(table, '\t'),
        f if f.starts_with("JSON") => render::json(table),
        _ => render::markdown(table),
    };
    vec![Output::new(text_format()).data(text.into_bytes()), Output::new(html_format()).data(render::html(table).into_bytes())]
}

impl NativePlugin for QueryPlugin {
    fn analyze(&self, entry: &ClipEntry) -> Result<AnalysisData, PluginError> {
        let Some(source) = source(entry) else {
            return Ok(AnalysisData::empty());
        };
        let mut data = AnalysisData::new().set("table", true);
        // Which saved queries make sense depends on the columns. For very
        // large files that is not worth opening them for; offer everything.
        let large = match &source {
            Source::Files { paths, .. } => paths.iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum::<u64>() > MAX_ANALYZE_BYTES,
            Source::Text { .. } => false,
        };
        if !large {
            match Db::open(&source).and_then(|db| db.columns()) {
                Ok(columns) => {
                    let offered: Vec<String> = MODES
                        .iter()
                        .filter(|m| **m == "sql" || engine::saved_query(m, &columns, file_count(&source)).is_some())
                        .map(|m| m.to_string())
                        .collect();
                    data = data.set("offered", offered);
                }
                // It looked like a table and would not load as one.
                Err(_) => return Ok(AnalysisData::empty()),
            }
        }
        Ok(data)
    }

    fn applies_to(&self, entry: &ClipEntry) -> bool {
        entry.analysis().has("table")
    }

    fn applies_to_mode(&self, entry: &ClipEntry, mode: &str) -> bool {
        match entry.analysis().get_cloned::<Vec<String>>("offered") {
            Some(offered) => offered.iter().any(|m| m == mode),
            None => entry.analysis().has("table"),
        }
    }

    fn apply(&self, entry: &ClipEntry, mode: Option<&str>) -> Result<Vec<Output>, PluginError> {
        let source = source(entry).ok_or_else(|| PluginError::from("This clip is not a table"))?;
        let table = run(&source, mode.unwrap_or("summary"), entry.input()).map_err(PluginError)?;
        let config = entry.config();
        let form = config.get("format").and_then(|v| v.as_str()).unwrap_or("Markdown table");
        Ok(outputs(&table, form))
    }
}

schrodinger_plugin!(QueryPlugin);

#[allow(dead_code)]
fn _columns_are_used(_: Column) {}

#[cfg(test)]
mod tests {
    use super::*;
    use source::TextKind;

    fn sales() -> Source {
        Source::Text { kind: TextKind::Delimited(','), text: "region,qty\nnorth,2\nsouth,5\nnorth,1\n".into() }
    }

    #[test]
    fn runs_saved_and_typed_queries() {
        assert_eq!(run(&sales(), "count", None).unwrap().single_value(), Some("3"));
        assert_eq!(run(&sales(), "sql", Some("sum(qty)")).unwrap().single_value(), Some("8"));
        assert_eq!(run(&sales(), "sql", Some("where qty > 1 order by qty")).unwrap().rows.len(), 2);
        assert!(run(&sales(), "sql", None).unwrap_err().contains("Type ="));
        assert!(run(&sales(), "sql", Some("  ")).is_err());
        assert!(run(&sales(), "combined", None).is_err());
    }

    #[test]
    fn a_lone_value_is_pasted_bare_and_a_table_in_two_forms() {
        let count = outputs(&run(&sales(), "count", None).unwrap(), "Markdown table");
        assert_eq!(count.len(), 1);
        assert_eq!(count[0].data, b"3");

        let grouped = run(&sales(), "group-totals", None).unwrap();
        let out = outputs(&grouped, "Markdown table");
        assert_eq!(out.len(), 2);
        assert_eq!(String::from_utf8_lossy(&out[0].data), "| region | qty |\n| --- | --- |\n| north | 3 |\n| south | 5 |");
        assert!(String::from_utf8_lossy(&out[1].data).starts_with("<table>"));
        assert_eq!(String::from_utf8_lossy(&outputs(&grouped, "Tab-separated")[0].data), "region\tqty\nnorth\t3\nsouth\t5");
        assert_eq!(String::from_utf8_lossy(&outputs(&grouped, "CSV")[0].data), "region,qty\nnorth,3\nsouth,5");
    }

    #[test]
    fn decodes_file_urls() {
        assert_eq!(path_from_file_url("file:///tmp/q1%20sales.csv"), Some(PathBuf::from("/tmp/q1 sales.csv")));
    }
}
