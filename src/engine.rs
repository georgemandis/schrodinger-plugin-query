//! Loading a clip into DuckDB and running queries on it.
//!
//! Each run gets a fresh in-memory database with one table, `clip`.

use crate::render::Table;
use crate::source::{FileKind, TextKind};
use duckdb::Connection;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum Source {
    Text { kind: TextKind, text: String },
    Files { kind: FileKind, paths: Vec<PathBuf> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub numeric: bool,
}

pub struct Db {
    conn: Connection,
    // The clip's text, written out for DuckDB to read; kept until the
    // database is dropped.
    _file: Option<tempfile::NamedTempFile>,
}

/// DuckDB's messages run to several lines of context. The first lines say
/// what is wrong.
fn brief(e: impl std::fmt::Display) -> String {
    let text = e.to_string();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(4).collect();
    lines.join("\n")
}

fn quote_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn is_numeric_type(t: &str) -> bool {
    let t = t.to_ascii_uppercase();
    ["TINYINT", "SMALLINT", "INTEGER", "BIGINT", "HUGEINT", "UTINYINT", "USMALLINT", "UINTEGER", "UBIGINT", "UHUGEINT", "FLOAT", "DOUBLE", "REAL"].contains(&t.as_str())
        || t.starts_with("DECIMAL")
        || t.starts_with("NUMERIC")
}

impl Db {
    pub fn open(source: &Source) -> Result<Db, String> {
        let conn = Connection::open_in_memory().map_err(brief)?;
        // Nothing is fetched behind the user's back: no extension is
        // downloaded unless a query asks for it by name.
        conn.execute_batch("SET autoinstall_known_extensions = false; SET autoload_known_extensions = false;").map_err(brief)?;

        let mut file = None;
        let reader = match source {
            Source::Text { kind, text } => {
                let (suffix, body, reader): (&str, std::borrow::Cow<str>, fn(&str) -> String) = match kind {
                    TextKind::Json => (".json", text.as_str().into(), |p| format!("read_json_auto({p})")),
                    TextKind::Markdown => (".tsv", crate::source::markdown_to_tsv(text).into(), |p| format!("read_csv({p}, delim = '\t')")),
                    TextKind::Delimited('\t') => (".tsv", text.as_str().into(), |p| format!("read_csv({p}, delim = '\t')")),
                    TextKind::Delimited(';') => (".csv", text.as_str().into(), |p| format!("read_csv({p}, delim = ';')")),
                    TextKind::Delimited('|') => (".csv", text.as_str().into(), |p| format!("read_csv({p}, delim = '|')")),
                    TextKind::Delimited(_) => (".csv", text.as_str().into(), |p| format!("read_csv({p}, delim = ',')")),
                };
                let mut temp = tempfile::Builder::new().prefix("schrodinger-query-").suffix(suffix).tempfile().map_err(brief)?;
                temp.write_all(body.as_bytes()).and_then(|_| temp.flush()).map_err(brief)?;
                let sql = reader(&quote_string(&temp.path().to_string_lossy()));
                file = Some(temp);
                sql
            }
            Source::Files { kind, paths } => {
                let list = paths.iter().map(|p| quote_string(&p.to_string_lossy())).collect::<Vec<_>>().join(", ");
                // Several files become one table; a column missing from
                // one of them is empty for its rows.
                match kind {
                    FileKind::Csv => format!("read_csv([{list}], union_by_name = true)"),
                    FileKind::Json => format!("read_json_auto([{list}], union_by_name = true)"),
                    FileKind::Parquet => format!("read_parquet([{list}], union_by_name = true)"),
                }
            }
        };
        conn.execute_batch(&format!("CREATE TABLE clip AS SELECT * FROM {reader};")).map_err(|e| format!("Could not read this as a table: {}", brief(e)))?;
        Ok(Db { conn, _file: file })
    }

    pub fn columns(&self) -> Result<Vec<Column>, String> {
        let described = self.run("SELECT column_name, column_type FROM (DESCRIBE clip)", 10_000)?;
        Ok(described
            .rows
            .into_iter()
            .map(|row| Column { name: row[0].clone().unwrap_or_default(), numeric: is_numeric_type(row[1].as_deref().unwrap_or("")) })
            .collect())
    }

    /// Run a query and return its rows as text. More than `max_rows` rows
    /// is an error, not a silent cut.
    pub fn run(&self, sql: &str, max_rows: usize) -> Result<Table, String> {
        let sql = sql.trim().trim_end_matches(';').trim();
        if sql.is_empty() {
            return Err("Nothing to run".into());
        }
        // Every column as text, so that any type can be read out the same way.
        let wrapped = format!("SELECT COLUMNS(*)::VARCHAR FROM ({sql}) AS result LIMIT {}", max_rows + 1);
        let mut statement = self.conn.prepare(&wrapped).map_err(brief)?;
        let mut rows = statement.query([]).map_err(brief)?;
        let mut columns: Vec<String> = Vec::new();
        let mut out: Vec<Vec<Option<String>>> = Vec::new();
        while let Some(row) = rows.next().map_err(brief)? {
            if columns.is_empty() {
                columns = row.as_ref().column_names();
            }
            let cells: Result<Vec<Option<String>>, _> = (0..columns.len()).map(|i| row.get::<_, Option<String>>(i)).collect();
            out.push(cells.map_err(brief)?);
            if out.len() > max_rows {
                return Err(format!("The result has more than {max_rows} rows. Narrow it down, or add a LIMIT."));
            }
        }
        if columns.is_empty() {
            columns = rows.as_ref().map(|s| s.column_names()).unwrap_or_default();
        }
        Ok(Table { columns, rows: out })
    }
}

/// The saved queries. `None` when a query has nothing to work on in this
/// table (totals with no numeric column), so that it is not offered.
pub fn saved_query(mode: &str, columns: &[Column], file_count: usize) -> Option<String> {
    let numeric: Vec<&Column> = columns.iter().filter(|c| c.numeric).collect();
    // The column to group by: the first that is not a number.
    let group = columns.iter().find(|c| !c.numeric);
    let each = |f: &dyn Fn(&str) -> String| numeric.iter().map(|c| format!("{} AS {}", f(&quote_ident(&c.name)), quote_ident(&c.name))).collect::<Vec<_>>().join(", ");
    Some(match mode {
        "summary" => "SELECT column_name AS \"column\", column_type AS \"type\", min, max, round(try_cast(avg AS DOUBLE), 2) AS average, approx_unique AS \"distinct\", count AS \"rows\", null_percentage AS \"empty %\" FROM (SUMMARIZE clip)".to_string(),
        "columns" => "SELECT column_name AS \"column\", column_type AS \"type\" FROM (DESCRIBE clip)".to_string(),
        "count" => "SELECT count(*) AS \"rows\" FROM clip".to_string(),
        "totals" if !numeric.is_empty() => format!("SELECT {} FROM clip", each(&|c| format!("sum({c})"))),
        "averages" if !numeric.is_empty() => format!("SELECT {} FROM clip", each(&|c| format!("round(avg({c}), 2)"))),
        "group-count" => {
            let g = quote_ident(&group?.name);
            format!("SELECT {g}, count(*) AS \"count\" FROM clip GROUP BY {g} ORDER BY \"count\" DESC, {g}")
        }
        "group-totals" if !numeric.is_empty() => {
            let g = quote_ident(&group?.name);
            format!("SELECT {g}, {} FROM clip GROUP BY {g} ORDER BY {g}", each(&|c| format!("sum({c})")))
        }
        "distinct" => "SELECT DISTINCT * FROM clip".to_string(),
        "sorted" => "SELECT * FROM clip ORDER BY 1".to_string(),
        "head" => "SELECT * FROM clip LIMIT 10".to_string(),
        "combined" if file_count >= 2 => "SELECT * FROM clip".to_string(),
        _ => return None,
    })
}

/// Turn what was typed into a full query. A whole statement is taken as it
/// is. Anything shorter is completed around the clip:
///
///   where qty > 1            SELECT * FROM clip where qty > 1
///   order by price desc      SELECT * FROM clip order by price desc
///   sum(price), max(qty)     SELECT sum(price), max(qty) FROM clip
pub fn expand(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches(';').trim();
    let first = trimmed.split(|c: char| c.is_whitespace() || c == '(').next().unwrap_or("").to_ascii_lowercase();
    match first.as_str() {
        "select" | "with" | "from" | "describe" | "summarize" | "pivot" | "unpivot" | "show" | "values" | "table" | "explain" => trimmed.to_string(),
        "where" | "order" | "group" | "limit" | "qualify" | "having" | "using" => format!("SELECT * FROM clip {trimmed}"),
        _ => {
            // "region, sum(qty) group by 1": the table goes before the
            // first clause, not at the end.
            let at = clause_start(trimmed).unwrap_or(trimmed.len());
            let (columns, clauses) = trimmed.split_at(at);
            format!("SELECT {} FROM clip {}", columns.trim(), clauses.trim()).trim_end().to_string()
        }
    }
}

/// Where the first clause keyword (where, group by, order by, ...) starts,
/// outside any parentheses and quotes.
fn clause_start(text: &str) -> Option<usize> {
    const CLAUSES: &[&str] = &["where", "group", "order", "having", "limit", "qualify", "window"];
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let (mut depth, mut quote) = (0i32, None::<u8>);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'\'' | b'"' => quote = Some(b),
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ if depth == 0 && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                    for clause in CLAUSES {
                        let end = i + clause.len();
                        if lower[i..].starts_with(clause) && bytes.get(end).map_or(true, |c| c.is_ascii_whitespace()) {
                            return Some(i);
                        }
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALES: &str = "region,product,qty,price\nnorth,widget,2,3.50\nsouth,widget,5,3.50\nnorth,gadget,1,12.00\nnorth,widget,2,3.50\n";

    fn db() -> Db {
        Db::open(&Source::Text { kind: TextKind::Delimited(','), text: SALES.to_string() }).unwrap()
    }

    fn rows(t: &Table) -> Vec<Vec<&str>> {
        t.rows.iter().map(|r| r.iter().map(|c| c.as_deref().unwrap_or("NULL")).collect()).collect()
    }

    #[test]
    fn loads_text_and_knows_its_columns() {
        let columns = db().columns().unwrap();
        assert_eq!(columns.iter().map(|c| (c.name.as_str(), c.numeric)).collect::<Vec<_>>(), vec![("region", false), ("product", false), ("qty", true), ("price", true)]);
    }

    #[test]
    fn saved_queries() {
        let db = db();
        let columns = db.columns().unwrap();
        let run = |mode: &str| db.run(&saved_query(mode, &columns, 1).unwrap(), 1000).unwrap();
        assert_eq!(run("count").single_value(), Some("4"));
        let totals = run("totals");
        assert_eq!(totals.columns, vec!["qty", "price"]);
        assert_eq!(rows(&totals), vec![vec!["10", "22.5"]]);
        assert_eq!(rows(&run("averages")), vec![vec!["2.5", "5.63"]]);
        assert_eq!(rows(&run("group-count")), vec![vec!["north", "3"], vec!["south", "1"]]);
        assert_eq!(rows(&run("group-totals")), vec![vec!["north", "5", "19.0"], vec!["south", "5", "3.5"]]);
        assert_eq!(run("distinct").rows.len(), 3);
        assert_eq!(run("head").rows.len(), 4);
        assert_eq!(run("columns").rows.len(), 4);
        let summary = run("summary");
        assert_eq!(summary.columns[0], "column");
        assert_eq!(summary.rows.len(), 4);
        // One file is not a combination.
        assert!(saved_query("combined", &columns, 1).is_none());
        assert!(saved_query("combined", &columns, 2).is_some());
    }

    #[test]
    fn queries_without_numbers_are_not_offered() {
        let db = Db::open(&Source::Text { kind: TextKind::Delimited(','), text: "name,city\nann,leeds\nbo,lyon\n".into() }).unwrap();
        let columns = db.columns().unwrap();
        assert!(saved_query("totals", &columns, 1).is_none());
        assert!(saved_query("group-totals", &columns, 1).is_none());
        assert!(saved_query("group-count", &columns, 1).is_some());
    }

    #[test]
    fn typed_queries_are_completed() {
        assert_eq!(expand("where qty > 1"), "SELECT * FROM clip where qty > 1");
        assert_eq!(expand("sum(price), max(qty)"), "SELECT sum(price), max(qty) FROM clip");
        assert_eq!(expand("select * from clip limit 2;"), "select * from clip limit 2");
        assert_eq!(expand("from clip select region"), "from clip select region");
        assert_eq!(expand("order by price desc"), "SELECT * FROM clip order by price desc");
        assert_eq!(expand("region, sum(qty) group by 1 order by 2 desc"), "SELECT region, sum(qty) FROM clip group by 1 order by 2 desc");
        // Keywords inside parentheses and quotes are not clauses.
        assert_eq!(expand("max(qty) filter (where qty < 5), 'order here'"), "SELECT max(qty) filter (where qty < 5), 'order here' FROM clip");
        assert_eq!(expand("count(*) where region = 'group one'"), "SELECT count(*) FROM clip where region = 'group one'");
        let db = db();
        assert_eq!(db.run(&expand("sum(qty * price)"), 10).unwrap().single_value(), Some("43.5"));
        assert_eq!(db.run(&expand("where region = 'south'"), 10).unwrap().rows.len(), 1);
        let by_region = db.run(&expand("region, sum(qty * price) as revenue group by 1 order by 2 desc"), 10).unwrap();
        assert_eq!(rows(&by_region), vec![vec!["north", "26.0"], vec!["south", "17.5"]]);
    }

    #[test]
    fn mistakes_and_floods_are_reported() {
        let db = db();
        let error = db.run("select nope from clip", 10).unwrap_err();
        assert!(error.contains("nope"), "{error}");
        assert!(error.lines().count() <= 4);
        let error = db.run("select * from clip", 2).unwrap_err();
        assert!(error.contains("more than 2 rows"), "{error}");
    }

    #[test]
    fn other_shapes_of_text() {
        let json = Db::open(&Source::Text { kind: TextKind::Json, text: r#"[{"a":1,"b":"x"},{"a":2,"b":"y"}]"#.into() }).unwrap();
        assert_eq!(json.run("select sum(a) from clip", 10).unwrap().single_value(), Some("3"));
        let md = Db::open(&Source::Text { kind: TextKind::Markdown, text: "| name | qty |\n|---|---|\n| widget | 2 |\n| gadget | 5 |".into() }).unwrap();
        assert_eq!(md.run("select name from clip where qty > 2", 10).unwrap().single_value(), Some("gadget"));
        let tsv = Db::open(&Source::Text { kind: TextKind::Delimited('\t'), text: "a\tb\n1\t2\n".into() }).unwrap();
        assert_eq!(tsv.columns().unwrap().len(), 2);
    }

    #[test]
    fn several_files_become_one_table() {
        let dir = tempfile::tempdir().unwrap();
        let (jan, feb) = (dir.path().join("jan.csv"), dir.path().join("feb.csv"));
        std::fs::write(&jan, "region,qty\nnorth,2\nsouth,5\n").unwrap();
        // A column the first file lacks, and the columns in another order.
        std::fs::write(&feb, "qty,region,note\n7,north,late\n").unwrap();
        let db = Db::open(&Source::Files { kind: FileKind::Csv, paths: vec![jan, feb] }).unwrap();
        let columns = db.columns().unwrap();
        assert_eq!(db.run(&saved_query("count", &columns, 2).unwrap(), 10).unwrap().single_value(), Some("3"));
        assert_eq!(rows(&db.run(&saved_query("group-totals", &columns, 2).unwrap(), 10).unwrap()), vec![vec!["north", "9"], vec!["south", "5"]]);
        assert_eq!(db.run("select count(note) from clip", 10).unwrap().single_value(), Some("1"));
    }
}
