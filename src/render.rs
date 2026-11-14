//! Writing a result out.

/// A query result: column names and rows of cells (None is NULL).
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

impl Table {
    /// One value and nothing else: a count, a total.
    pub fn single_value(&self) -> Option<&str> {
        match (self.columns.len(), self.rows.as_slice()) {
            (1, [row]) => row[0].as_deref(),
            _ => None,
        }
    }
}

fn cell(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("")
}

pub fn markdown(t: &Table) -> String {
    let clean = |s: &str| s.replace('|', "\\|").replace('\n', " ");
    let mut out = format!("| {} |\n|{}|\n", t.columns.iter().map(|c| clean(c)).collect::<Vec<_>>().join(" | "), t.columns.iter().map(|_| " --- ").collect::<Vec<_>>().join("|"));
    for row in &t.rows {
        out.push_str(&format!("| {} |\n", row.iter().map(|c| clean(cell(c))).collect::<Vec<_>>().join(" | ")));
    }
    out.trim_end().to_string()
}

pub fn delimited(t: &Table, delimiter: char) -> String {
    let field = |s: &str| {
        if s.contains(delimiter) || s.contains('"') || s.contains('\n') || s.contains('\r') {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    };
    let d = delimiter.to_string();
    let mut lines = vec![t.columns.iter().map(|c| field(c)).collect::<Vec<_>>().join(&d)];
    lines.extend(t.rows.iter().map(|row| row.iter().map(|c| field(cell(c))).collect::<Vec<_>>().join(&d)));
    lines.join("\n")
}

/// Numbers and booleans are written as such; everything else as a string.
pub fn json(t: &Table) -> String {
    let rows: Vec<serde_json::Value> = t
        .rows
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::new();
            for (name, value) in t.columns.iter().zip(row) {
                let v = match value {
                    None => serde_json::Value::Null,
                    Some(s) => match s.as_str() {
                        "true" => serde_json::Value::Bool(true),
                        "false" => serde_json::Value::Bool(false),
                        _ => s
                            .parse::<i64>()
                            .map(serde_json::Value::from)
                            .or_else(|_| s.parse::<f64>().ok().filter(|f| f.is_finite()).map(serde_json::Value::from).ok_or(()))
                            .unwrap_or_else(|_| serde_json::Value::String(s.clone())),
                    },
                };
                object.insert(name.clone(), v);
            }
            serde_json::Value::Object(object)
        })
        .collect();
    serde_json::to_string_pretty(&rows).unwrap_or_default()
}

/// A plain HTML table, for apps that paste rich text (documents,
/// spreadsheets, mail).
pub fn html(t: &Table) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut out = String::from("<table><thead><tr>");
    for c in &t.columns {
        out.push_str(&format!("<th>{}</th>", esc(c)));
    }
    out.push_str("</tr></thead><tbody>");
    for row in &t.rows {
        out.push_str("<tr>");
        for c in row {
            out.push_str(&format!("<td>{}</td>", esc(cell(c))));
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Table {
        Table {
            columns: vec!["name".into(), "qty".into(), "note".into()],
            rows: vec![vec![Some("widget".into()), Some("2".into()), Some("a|b, \"c\"".into())], vec![Some("gadget".into()), Some("1.5".into()), None]],
        }
    }

    #[test]
    fn writes_each_format() {
        let t = table();
        assert_eq!(markdown(&t), "| name | qty | note |\n| --- | --- | --- |\n| widget | 2 | a\\|b, \"c\" |\n| gadget | 1.5 |  |");
        assert_eq!(delimited(&t, ','), "name,qty,note\nwidget,2,\"a|b, \"\"c\"\"\"\ngadget,1.5,");
        assert_eq!(delimited(&t, '\t'), "name\tqty\tnote\nwidget\t2\t\"a|b, \"\"c\"\"\"\ngadget\t1.5\t");
        let parsed: serde_json::Value = serde_json::from_str(&json(&t)).unwrap();
        assert_eq!(parsed[0]["qty"], 2);
        assert_eq!(parsed[1]["qty"], 1.5);
        assert!(parsed[1]["note"].is_null());
        assert!(html(&t).contains("<th>name</th>") && html(&t).contains("<td>a|b, \"c\"</td>"));
    }

    #[test]
    fn a_lone_value_is_recognized() {
        let one = Table { columns: vec!["rows".into()], rows: vec![vec![Some("42".into())]] };
        assert_eq!(one.single_value(), Some("42"));
        assert_eq!(table().single_value(), None);
    }
}
