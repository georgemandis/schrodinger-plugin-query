# Query Plugin for Schrödinger

Run SQL over a copied table and paste the answer. Powered by [DuckDB](https://duckdb.org), which is built into the plugin; nothing is installed and nothing leaves your machine.

Copy some rows from a spreadsheet, and paste their totals. Copy three monthly CSV files in Finder, and paste one table of sums by region.

## What counts as a table

- Rows copied from a spreadsheet or a CSV (tab, comma, semicolon or pipe separated)
- A Markdown table
- JSON: an array of objects, or one object per line
- Copied data files: CSV/TSV, JSON/NDJSON, Parquet. Several files of the same kind are read as **one table**; a column that one file lacks is empty for that file's rows.

Text that merely has commas in it is not a table: every line has to have the same number of separators.

## Saved queries

| Format | What you get |
|---|---|
| **Summary** | One row per column: type, min, max, average, distinct values, empty share |
| **Columns and types** | The column names and what DuckDB took each to be |
| **Row count** | The number |
| **Totals** | The sum of every numeric column |
| **Averages** | The average of every numeric column, to two decimals |
| **Count by first column** | How many rows per value of the first text column, most frequent first |
| **Totals by first column** | Every numeric column summed per value of the first text column |
| **Unique rows** | The table without duplicate rows |
| **Sorted by first column** | |
| **First 10 rows** | |
| **Files combined** | Several copied files as one table |

A format is offered only where it has something to work on: no **Totals** for a table without numbers, **Files combined** only for more than one file.

## Your own query

Type `=` in the picker's search field, then a query. The clip is a table named `clip`. The preview shows the result as you type (or what is wrong with the query), and Enter pastes it.

A whole statement runs as it is. Anything shorter is completed around the clip:

| You type | What runs |
|---|---|
| `=sum(price)` | `SELECT sum(price) FROM clip` |
| `=region, sum(qty) group by 1` | `SELECT region, sum(qty) FROM clip group by 1` |
| `=where qty > 1` | `SELECT * FROM clip where qty > 1` |
| `=order by price desc limit 5` | `SELECT * FROM clip order by price desc limit 5` |
| `=select * from clip join 'prices.csv' using (sku)` | as typed |
| `=summarize clip` | as typed |

It is the full DuckDB dialect, including reading other files by path. Clearing the field goes back to searching your history. While a query is in the field, left and right move the caret; up and down still change the format.

## What gets pasted

- A single value (a count, a total) is pasted as just that value.
- A table is pasted as text in the form you choose in the settings (Markdown table, tab-separated, CSV or JSON), with a real HTML table alongside it, so documents, spreadsheets and mail get a table whatever the text form is.

## Limits

- A result of more than 10,000 rows is refused with a message; narrow the query or add a `LIMIT`.
- A paste has about 30 seconds. Very large files may not finish.
- Types are DuckDB's guesses from the data. `=describe clip` (or **Columns and types**) shows them; cast in your query where a guess is wrong.
- A clip is read fresh for each query. Nothing persists between them.
- Extensions are not downloaded automatically. A query can still `INSTALL` one by name.
- The plugin is large (tens of megabytes), because DuckDB is inside it.

## Building

```sh
cargo test --release     # release, so that DuckDB is compiled once
sh scripts/package.sh    # stages dist/query/
```

The first build compiles DuckDB from source and takes several minutes.

Needs version 0.3.0 of the plugin SDK (for typed input) and a Schrödinger that supports prompt modes.

## License

MIT
