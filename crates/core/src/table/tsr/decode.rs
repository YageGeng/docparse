//! Strict, bounded decoding of the restricted table-structure token grammar.
use docparse_layout::{Bbox, Point, Quad};

use super::{TableStructureError, TsrTableInput, TsrTableRequest};
use crate::table::grid::CellGrid;
use crate::table::{
    MAX_TABLE_CELLS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, Table, TableCell,
    TableStructureSource,
};

/// Only positive numeric spans are accepted from external structure attributes.
enum SpanAttribute {
    Row(usize),
    Column(usize),
}

impl TryFrom<&str> for SpanAttribute {
    type Error = String;

    /// Parses an exact quoted span attribute; arbitrary HTML attributes are rejected.
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let (name, quoted) = raw
            .split_once('=')
            .ok_or("expected a quoted span attribute")?;
        let value = quoted
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| {
                quoted.strip_prefix('\'').and_then(|v| v.strip_suffix('\''))
            })
            .ok_or("span attribute must be quoted")?;
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err("span must be a positive integer".to_owned());
        }
        let span = value
            .parse::<usize>()
            .map_err(|_overflow| "span integer overflow")?;
        match name {
            "rowspan" if (1..=MAX_TABLE_ROWS).contains(&span) => {
                Ok(Self::Row(span))
            }
            "colspan" if (1..=MAX_TABLE_COLUMNS).contains(&span) => {
                Ok(Self::Column(span))
            }
            _ => Err("unknown or out-of-range span attribute".to_owned()),
        }
    }
}

impl TryFrom<(&TsrTableRequest, Bbox, TsrTableInput)> for CellGrid {
    type Error = TableStructureError;

    /// Resolves a response against its crop and frozen layout bounds, then validates grid occupancy.
    fn try_from(
        (request, table_bbox, input): (&TsrTableRequest, Bbox, TsrTableInput),
    ) -> Result<Self, Self::Error> {
        StructureDecoder::decode(request, table_bbox, input)
            .map_err(|reason| TableStructureError::InvalidInput { reason })
    }
}

/// Restricted token decoder, independent of text extraction and local topology inference.
struct StructureDecoder;

impl StructureDecoder {
    /// Accepts wrapper/section/row/cell tags and bounded rowspan/colspan fragments only.
    fn decode(
        request: &TsrTableRequest,
        table_bbox: Bbox,
        input: TsrTableInput,
    ) -> Result<CellGrid, String> {
        if request.request_id != input.request_id {
            return Err(
                "response request_id does not match its crop".to_owned()
            );
        }
        if input.structure_tokens.is_empty()
            || input.structure_tokens.len() > MAX_TABLE_CELLS * 16
            || input.cell_bboxes.is_empty()
            || input.cell_bboxes.len() > MAX_TABLE_CELLS
            || input
                .structure_tokens
                .iter()
                .try_fold(0usize, |n, t| n.checked_add(t.len()))
                .is_none_or(|n| n > 1_048_576)
        {
            return Err("external structure exceeds bounded token/cell limits"
                .to_owned());
        }
        request
            .crop_to_viewport
            .inverse()
            .map_err(|e| e.to_string())?;
        let table_bbox = Bbox::try_from([
            table_bbox.left,
            table_bbox.top,
            table_bbox.right,
            table_bbox.bottom,
        ])
        .map_err(|e| e.to_string())?;
        let mut cells = Vec::new();
        let mut occupied = vec![false; MAX_TABLE_CELLS];
        let mut stack: Vec<&str> = Vec::new();
        let mut row_count = 0usize;
        let mut column = 0usize;
        let mut column_count = 0usize;
        let mut tokens = input.structure_tokens.iter();
        while let Some(raw) = tokens.next() {
            let token = raw.trim();
            match token {
                "<html>" | "<body>" | "<table>" | "<thead>" | "<tbody>"
                | "<tfoot>" => {
                    let tag =
                        token.trim_start_matches('<').trim_end_matches('>');
                    let allowed = match tag {
                        "html" => stack.is_empty() && row_count == 0,
                        "body" => {
                            stack.last() == Some(&"html") && row_count == 0
                        }
                        "table" => {
                            matches!(stack.last(), None | Some(&"body"))
                                && row_count == 0
                        }
                        _ => matches!(stack.last(), None | Some(&"table")),
                    };
                    if !allowed || stack.len() >= 6 {
                        return Err("invalid table wrapper nesting".to_owned());
                    }
                    stack.push(tag);
                }
                "<tr>" => {
                    if !matches!(
                        stack.last(),
                        None | Some(&"table")
                            | Some(&"thead")
                            | Some(&"tbody")
                            | Some(&"tfoot")
                    ) || row_count >= MAX_TABLE_ROWS
                    {
                        return Err("invalid or excessive table row".to_owned());
                    }
                    stack.push("tr");
                    row_count += 1;
                    column = 0;
                }
                "</html>" | "</body>" | "</table>" | "</thead>"
                | "</tbody>" | "</tfoot>" | "</tr>" | "</td>" | "</th>" => {
                    let tag =
                        token.trim_start_matches("</").trim_end_matches('>');
                    if stack.pop() != Some(tag) {
                        return Err(
                            "unbalanced table structure tokens".to_owned()
                        );
                    }
                }
                "<td></td>" | "<th></th>" | "<td>" | "<th>" | "<td" | "<th" => {
                    if stack.last() != Some(&"tr") {
                        return Err("cell must be inside a row".to_owned());
                    }
                    let row =
                        row_count.checked_sub(1).ok_or("cell without a row")?;
                    let is_th = token.starts_with("<th");
                    let mut row_span = None;
                    let mut column_span = None;
                    if token == "<td" || token == "<th" {
                        let mut closed = false;
                        for fragment in tokens.by_ref() {
                            if fragment.trim() == ">" {
                                closed = true;
                                break;
                            }
                            for attribute in fragment.split_ascii_whitespace() {
                                match SpanAttribute::try_from(attribute)? {
                                    SpanAttribute::Row(n)
                                        if row_span.replace(n).is_none() => {}
                                    SpanAttribute::Column(n)
                                        if column_span.replace(n).is_none() => {
                                    }
                                    _ => {
                                        return Err("duplicate span attribute"
                                            .to_owned());
                                    }
                                }
                            }
                        }
                        if !closed {
                            return Err(
                                "unterminated cell attributes".to_owned()
                            );
                        }
                    }
                    let row_span = row_span.unwrap_or(1);
                    let column_span = column_span.unwrap_or(1);
                    while column < MAX_TABLE_COLUMNS
                        && occupied.get(row * MAX_TABLE_COLUMNS + column)
                            == Some(&true)
                    {
                        column += 1;
                    }
                    let end_column = column
                        .checked_add(column_span)
                        .ok_or("column span overflow")?;
                    let end_row =
                        row.checked_add(row_span).ok_or("row span overflow")?;
                    if end_column > MAX_TABLE_COLUMNS
                        || end_row > MAX_TABLE_ROWS
                        || end_row
                            .checked_mul(end_column)
                            .is_none_or(|n| n > MAX_TABLE_CELLS)
                    {
                        return Err("cell exceeds table dimensions".to_owned());
                    }
                    // The occupancy scratch uses a fixed 64-column stride, bounded independently of final dimensions.
                    let required = end_row * MAX_TABLE_COLUMNS;
                    if required > occupied.len() {
                        occupied.resize(required, false);
                    }
                    for r in row..end_row {
                        for c in column..end_column {
                            let slot = occupied
                                .get_mut(r * MAX_TABLE_COLUMNS + c)
                                .ok_or("cell occupancy overflow")?;
                            if std::mem::replace(slot, true) {
                                return Err("external cells overlap in grid coordinates".to_owned());
                            }
                        }
                    }
                    let raw_box = input
                        .cell_bboxes
                        .get(cells.len())
                        .ok_or("fewer boxes than cell tokens")?;
                    let bbox = Self::cell_box(request, table_bbox, raw_box)?;
                    cells.push(
                        TableCell::builder()
                            .row(row)
                            .column(column)
                            .row_span(row_span)
                            .column_span(column_span)
                            .is_header(is_th || stack.contains(&"thead"))
                            .bbox(Some(bbox))
                            .build(),
                    );
                    if cells.len() > MAX_TABLE_CELLS {
                        return Err("too many external cells".to_owned());
                    }
                    column_count = column_count.max(end_column);
                    column = end_column;
                    if token != "<td></td>" && token != "<th></th>" {
                        stack.push(if is_th { "th" } else { "td" });
                    }
                }
                _ => return Err("unsupported table structure token".to_owned()),
            }
        }
        if !stack.is_empty() || cells.len() != input.cell_bboxes.len() {
            return Err("unbalanced tokens or unmatched cell boxes".to_owned());
        }
        CellGrid::try_from(
            Table::builder()
                .row_count(row_count)
                .column_count(column_count)
                .cells(cells)
                .source(TableStructureSource::ExternalTsr)
                .build(),
        )
    }

    /// Validates crop pixels, transforms their corners, and removes sampling margins outside the layout.
    fn cell_box(
        request: &TsrTableRequest,
        table_bbox: Bbox,
        raw: &[f64],
    ) -> Result<Bbox, String> {
        let pixel = match *raw {
            [left, top, right, bottom] => {
                Bbox::try_from([left, top, right, bottom])
                    .map_err(|e| e.to_string())?
            }
            [x0, y0, x1, y1, x2, y2, x3, y3] => {
                let quad = Quad::try_from([
                    Point::new(x0, y0),
                    Point::new(x1, y1),
                    Point::new(x2, y2),
                    Point::new(x3, y3),
                ])
                .map_err(|e| e.to_string())?;
                let points = quad.points();
                Bbox::try_from([
                    points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min),
                    points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min),
                    points
                        .iter()
                        .map(|p| p.x)
                        .fold(f64::NEG_INFINITY, f64::max),
                    points
                        .iter()
                        .map(|p| p.y)
                        .fold(f64::NEG_INFINITY, f64::max),
                ])
                .map_err(|e| e.to_string())?
            }
            _ => {
                return Err(
                    "a cell box needs four or eight coordinates".to_owned()
                );
            }
        };
        let width = f64::from(request.image.width());
        let height = f64::from(request.image.height());
        if pixel.left < -1.0
            || pixel.top < -1.0
            || pixel.right > width + 1.0
            || pixel.bottom > height + 1.0
        {
            return Err("cell box extends outside its crop".to_owned());
        }
        let corners = [
            Point::new(pixel.left.max(0.0), pixel.top.max(0.0)),
            Point::new(pixel.right.min(width), pixel.top.max(0.0)),
            Point::new(pixel.right.min(width), pixel.bottom.min(height)),
            Point::new(pixel.left.max(0.0), pixel.bottom.min(height)),
        ]
        .map(|p| request.crop_to_viewport.transform_point(p));
        let bbox = Bbox::try_from([
            corners.iter().map(|p| p.x).fold(f64::INFINITY, f64::min),
            corners.iter().map(|p| p.y).fold(f64::INFINITY, f64::min),
            corners
                .iter()
                .map(|p| p.x)
                .fold(f64::NEG_INFINITY, f64::max),
            corners
                .iter()
                .map(|p| p.y)
                .fold(f64::NEG_INFINITY, f64::max),
        ])
        .map_err(|e| e.to_string())?;
        let limit = request.crop_bbox;
        if bbox.left < limit.left - 1e-6
            || bbox.top < limit.top - 1e-6
            || bbox.right > limit.right + 1e-6
            || bbox.bottom > limit.bottom + 1e-6
        {
            return Err(
                "transformed cell lies outside its table region".to_owned()
            );
        }
        // Raster rounding may sample outside the semantic table. Only trim that outer margin;
        // internal boundaries and spans stay as declared, and cells cannot disappear silently.
        Bbox::try_from([
            bbox.left.max(limit.left).max(table_bbox.left),
            bbox.top.max(limit.top).max(table_bbox.top),
            bbox.right.min(limit.right).min(table_bbox.right),
            bbox.bottom.min(limit.bottom).min(table_bbox.bottom),
        ])
        .map_err(|_empty_intersection| {
            "cell has no positive area inside its table region".to_owned()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockId, TsrRequestReason};
    use docparse_layout::{
        AffineTransform, PageImage, PageImageInput, PixelFormat,
    };
    use std::sync::Arc;

    /// Creates a real pixel-to-viewport transform with a nonzero crop origin.
    fn request() -> TsrTableRequest {
        TsrTableRequest::builder()
            .request_id("test:1".to_owned())
            .page_number(1)
            .block_id(BlockId::model(1, 0, 0))
            .crop_bbox(Bbox::try_from([10.0, 20.0, 110.0, 70.0]).expect("crop"))
            .image(Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(200)
                        .height(100)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(vec![255; 60000]))
                        .build(),
                )
                .expect("image"),
            ))
            .crop_to_viewport(
                AffineTransform::builder()
                    .a(0.5)
                    .b(0.0)
                    .c(0.0)
                    .d(0.5)
                    .e(10.0)
                    .f(20.0)
                    .build(),
            )
            .reason(TsrRequestReason::ExternalOnly)
            .build()
    }

    /// Builds an explicit two-row grid whose first cell owns a rowspan.
    fn input() -> TsrTableInput {
        TsrTableInput::builder()
            .request_id("test:1".to_owned())
            .structure_tokens(
                [
                    "<table>",
                    "<tbody>",
                    "<tr>",
                    "<td",
                    " rowspan=\"2\"",
                    ">",
                    "</td>",
                    "<th></th>",
                    "</tr>",
                    "<tr>",
                    "<td></td>",
                    "</tr>",
                    "</tbody>",
                    "</table>",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            )
            .cell_bboxes(vec![
                vec![0.0, 0.0, 100.0, 100.0],
                vec![100.0, 0.0, 200.0, 0.0, 200.0, 50.0, 100.0, 50.0],
                vec![100.0, 50.0, 200.0, 100.0],
            ])
            .build()
    }

    /// Spans occupy one owner and all coordinates use the actual crop transform.
    #[test]
    fn external_topology_and_crop_geometry_are_preserved() {
        let request = request();
        let grid = CellGrid::try_from((&request, request.crop_bbox, input()))
            .expect("external grid");
        assert_eq!(
            (
                grid.table().row_count,
                grid.table().column_count,
                grid.table().cells.len()
            ),
            (2, 2, 3)
        );
        let first = grid.table().cells.first().expect("cell");
        assert_eq!(
            (first.row_span, first.column_span, first.is_header),
            (2, 1, false)
        );
        assert_eq!(
            first.bbox,
            Some(Bbox::try_from([10.0, 20.0, 60.0, 70.0]).expect("box"))
        );
        assert!(grid.table().cells.get(1).expect("header").is_header);
    }

    /// Trimming crop margins leaves internal dividers, explicit empty cells, and declared spans intact.
    #[test]
    fn table_clipping_preserves_internal_geometry_and_spans() {
        let request = request();
        let bounds = Bbox::try_from([10.2, 20.2, 109.8, 69.8]).expect("layout");
        let grid = CellGrid::try_from((&request, bounds, input()))
            .expect("clipped grid");
        assert_eq!((grid.table().row_count, grid.table().column_count), (2, 2));
        let actual = grid
            .table()
            .cells
            .iter()
            .map(|c| {
                (
                    c.row,
                    c.column,
                    c.row_span,
                    c.column_span,
                    c.is_header,
                    c.bbox,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            [
                (
                    0,
                    0,
                    2,
                    1,
                    false,
                    Some(
                        Bbox::try_from([10.2, 20.2, 60.0, 69.8])
                            .expect("spanning cell")
                    )
                ),
                (
                    0,
                    1,
                    1,
                    1,
                    true,
                    Some(
                        Bbox::try_from([60.0, 20.2, 109.8, 45.0])
                            .expect("header")
                    )
                ),
                (
                    1,
                    1,
                    1,
                    1,
                    false,
                    Some(
                        Bbox::try_from([60.0, 45.0, 109.8, 69.8])
                            .expect("body")
                    )
                ),
            ]
        );
    }

    /// A cell wholly in the sampling margin is rejected instead of being dropped or collapsed.
    #[test]
    fn table_clipping_rejects_cells_without_positive_intersection() {
        let request = request();
        for left in [60.0, 60.1] {
            let bounds =
                Bbox::try_from([left, 20.0, 110.0, 70.0]).expect("layout");
            assert!(matches!(
                CellGrid::try_from((&request, bounds, input())),
                Err(TableStructureError::InvalidInput { .. })
            ));
        }
    }

    /// Invalid structural payloads cannot bypass crop bounds or occupancy limits.
    #[test]
    #[allow(
        clippy::indexing_slicing,
        reason = "mutations address the fixed fixture shape"
    )]
    fn malformed_external_structures_are_rejected() {
        let request = request();
        for changed in 0..12 {
            let mut input = input();
            match changed {
                0 => input.request_id = "stale".to_owned(),
                1 => input.structure_tokens.insert(0, "<script>".to_owned()),
                2 => input.structure_tokens[4] = " rowspan=\"0\"".to_owned(),
                3 => {
                    input.structure_tokens[4] =
                        " rowspan=\"999999999999999999999\"".to_owned()
                }
                4 => {
                    input.cell_bboxes.pop();
                }
                5 => input.cell_bboxes[0][0] = f64::NAN,
                6 => input.cell_bboxes[0][2] = 500.0,
                7 => input.structure_tokens[4] = " style=\"1\"".to_owned(),
                8 => {
                    input.structure_tokens.pop();
                }
                9 => input.structure_tokens[4] = " rowspan=\"3\"".to_owned(),
                10 => input.structure_tokens[4] = " rowspan=\"1\"".to_owned(),
                _ => {
                    input.structure_tokens[4] =
                        " rowspan=\"2\" rowspan=\"2\"".to_owned()
                }
            }
            assert!(
                CellGrid::try_from((&request, request.crop_bbox, input))
                    .is_err(),
                "mutation {changed}"
            );
        }
    }
}
