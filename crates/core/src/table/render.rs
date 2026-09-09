use super::{
    MAX_TABLE_CELLS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, Table, TableCell,
};

impl Table {
    /// Projects logical rows to TSV, leaving covered merged-cell positions empty.
    pub fn to_text(&self) -> String {
        if self.row_count > MAX_TABLE_ROWS
            || self.column_count > MAX_TABLE_COLUMNS
            || self.row_count.saturating_mul(self.column_count)
                > MAX_TABLE_CELLS
        {
            return String::new();
        }
        let mut rows =
            vec![vec![String::new(); self.column_count]; self.row_count];
        for cell in &self.cells {
            if let Some(slot) = rows
                .get_mut(cell.row)
                .and_then(|row| row.get_mut(cell.column))
            {
                *slot = cell
                    .lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
            }
        }
        rows.into_iter()
            .map(|row| row.join("\t"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Uses pipe tables only when Markdown can represent every cell and its header semantics.
    pub fn to_markdown(&self) -> String {
        if self.row_count == 0
            || self.column_count == 0
            || self.row_count > MAX_TABLE_ROWS
            || self.column_count > MAX_TABLE_COLUMNS
            || self.row_count.saturating_mul(self.column_count)
                > MAX_TABLE_CELLS
        {
            return String::new();
        }
        let simple = self.cells.len() == self.row_count * self.column_count
            && self.cells.iter().all(|cell| {
                cell.row_span == 1
                    && cell.column_span == 1
                    && cell.is_header == (cell.row == 0)
            });
        if !simple {
            let mut output = String::from("<table>\n");
            for row in 0..self.row_count {
                output.push_str("<tr>");
                let mut cells: Vec<_> =
                    self.cells.iter().filter(|cell| cell.row == row).collect();
                cells.sort_by_key(|cell| cell.column);
                for cell in cells {
                    let tag = if cell.is_header { "th" } else { "td" };
                    output.push_str(&format!("<{tag}"));
                    if cell.row_span > 1 {
                        output.push_str(&format!(
                            " rowspan=\"{}\"",
                            cell.row_span
                        ));
                    }
                    if cell.column_span > 1 {
                        output.push_str(&format!(
                            " colspan=\"{}\"",
                            cell.column_span
                        ));
                    }
                    output.push('>');
                    output.push_str(&cell.escaped_text(false));
                    output.push_str(&format!("</{tag}>"));
                }
                output.push_str("</tr>\n");
            }
            output.push_str("</table>");
            return output;
        }
        let mut rows =
            vec![vec![String::new(); self.column_count]; self.row_count];
        for cell in &self.cells {
            if let Some(slot) = rows
                .get_mut(cell.row)
                .and_then(|row| row.get_mut(cell.column))
            {
                *slot = cell.escaped_text(true);
            }
        }
        let mut output = Vec::new();
        for (index, row) in rows.into_iter().enumerate() {
            output.push(format!("| {} |", row.join(" | ")));
            if index == 0 {
                output.push(format!(
                    "| {} |",
                    vec!["---"; self.column_count].join(" | ")
                ));
            }
        }
        output.join("\n")
    }
}

impl TableCell {
    /// Escapes untrusted source text without allowing HTML or Markdown syntax to change the grid.
    fn escaped_text(&self, markdown: bool) -> String {
        let mut text = String::new();
        for ch in self.text.chars() {
            match ch {
                '&' => text.push_str("&amp;"),
                '<' => text.push_str("&lt;"),
                '>' => text.push_str("&gt;"),
                '\n' => text.push_str("<br>"),
                '|' if markdown => text.push_str("&#124;"),
                '\\' | '*' | '_' | '[' | ']' | '`' if markdown => {
                    text.push('\\');
                    text.push(ch);
                }
                _ => text.push(ch),
            }
        }
        text
    }
}
