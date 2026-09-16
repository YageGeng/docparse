use docparse_core::{Table, TableCell, TableStructureSource};

/// Exported pipe tables preserve TeX operators, while prose and raw HTML tables remain escaped.
#[test]
fn table_formula_markdown_preserves_math_metacharacters() {
    let values = [
        "Expression",
        "$a < b$",
        "$a > b$",
        "$|x|$",
        r"$\|x\|$",
        r"$\begin{matrix}a & b\\ c & d\end{matrix}$",
        r"plain <b> & \*literal\*",
    ];
    let cells = values
        .into_iter()
        .enumerate()
        .map(|(row, value)| {
            TableCell::builder()
                .row(row)
                .column(0)
                .is_header(row == 0)
                .text(value.to_owned())
                .markdown(Some(value.to_owned()))
                .build()
        })
        .collect();
    let mut table = Table::builder()
        .row_count(values.len())
        .column_count(1)
        .cells(cells)
        .source(TableStructureSource::TextAlignment)
        .build();
    let markdown = table.to_markdown();
    assert_eq!(
        markdown,
        concat!(
            "| Expression |\n| --- |\n",
            "| $a < b$ |\n| $a > b$ |\n",
            "| $\\|x\\|$ |\n| $\\\\|x\\\\|$ |\n",
            "| $\\begin{matrix}a & b\\\\ c & d\\end{matrix}$ |\n",
            "| plain &lt;b&gt; &amp; \\*literal\\* |",
        )
    );
    // Feed the actual Rust output to both shipping renderers in the cross-language regression.
    if let Ok(path) = std::env::var("FORMULA_TABLE_MARKDOWN_OUTPUT") {
        std::fs::write(path, &markdown).expect("exported table Markdown");
    }
    table.cells.first_mut().expect("header").is_header = false;
    let html = table.to_markdown();
    assert!(html.contains("$a &lt; b$"));
    assert!(html.contains("$a &gt; b$"));
    assert!(html.contains("plain &lt;b&gt; &amp; *literal*"));
    assert!(!html.contains("<b>"));
    assert_eq!(
        table
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<Vec<_>>(),
        values
    );
}
