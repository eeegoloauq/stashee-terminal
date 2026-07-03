//! Pure tiling math: how `n` panes split into rows and cells.
//! See "Layout algorithm" in docs/SPEC.md for the contract.

/// Panes per row for `n` panes: `ceil(n / 3)` rows, fuller rows first,
/// row lengths differing by at most one. `layout(7) == [3, 2, 2]`.
#[must_use]
pub fn layout(n: usize) -> Vec<usize> {
    if n == 0 {
        return Vec::new();
    }
    let rows = n.div_ceil(3);
    let base = n / rows;
    let extra = n % rows;
    (0..rows)
        .map(|row| if row < extra { base + 1 } else { base })
        .collect()
}

/// A focus-move direction on the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The pane one step in `direction` from `index` in an `n`-pane grid,
/// `None` at the grid's edge. Horizontal moves walk pane order, so they
/// wrap onto the previous/next row. Vertical moves keep the column,
/// clamped to the target row's width.
#[must_use]
pub fn neighbor(n: usize, index: usize, direction: Direction) -> Option<usize> {
    let rows = layout(n);
    let mut start = 0;
    for (row, &len) in rows.iter().enumerate() {
        if index >= start + len {
            start += len;
            continue;
        }
        let col = index - start;
        return match direction {
            Direction::Left => index.checked_sub(1),
            Direction::Right => (index + 1 < n).then_some(index + 1),
            Direction::Up => row.checked_sub(1).map(|above| {
                let len = rows[above];
                start - len + col.min(len - 1)
            }),
            Direction::Down => (row + 1 < rows.len()).then(|| {
                let len = rows[row + 1];
                start + rows[row] + col.min(len - 1)
            }),
        };
    }
    None
}

/// Normalized cell rectangle in `[0, 1] × [0, 1]` coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Cell rects for `n` panes, in pane order (row-major). All rows share
/// one height; panes within a row share one width.
#[must_use]
#[allow(clippy::cast_precision_loss)] // pane counts are tiny
pub fn cells(n: usize) -> Vec<Cell> {
    let rows = layout(n);
    let height = 1.0 / rows.len().max(1) as f64;
    let mut out = Vec::with_capacity(n);
    for (row, &len) in rows.iter().enumerate() {
        let width = 1.0 / len as f64;
        for col in 0..len {
            out.push(Cell {
                x: col as f64 * width,
                y: row as f64 * height,
                width,
                height,
            });
        }
    }
    out
}

/// Pixel rectangle for `cell` inside a `width × height` area: `gap`
/// pixels of outer margin, and a `gap`-wide gutter between neighbours
/// (half taken from each side that touches another pane).
#[must_use]
pub fn pixel(cell: Cell, width: f64, height: f64, gap: f64) -> Cell {
    const EPS: f64 = 1e-9;
    let inner_w = (width - 2.0 * gap).max(0.0);
    let inner_h = (height - 2.0 * gap).max(0.0);
    let mut x = gap + cell.x * inner_w;
    let mut y = gap + cell.y * inner_h;
    let mut w = cell.width * inner_w;
    let mut h = cell.height * inner_h;
    if cell.x > EPS {
        x += gap / 2.0;
        w -= gap / 2.0;
    }
    if cell.x + cell.width < 1.0 - EPS {
        w -= gap / 2.0;
    }
    if cell.y > EPS {
        y += gap / 2.0;
        h -= gap / 2.0;
    }
    if cell.y + cell.height < 1.0 - EPS {
        h -= gap / 2.0;
    }
    Cell {
        x,
        y,
        width: w.max(0.0),
        height: h.max(0.0),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const EPS: f64 = 1e-9;

    #[test]
    fn rows_match_spec_table() {
        let table: &[(usize, &[usize])] = &[
            (0, &[]),
            (1, &[1]),
            (2, &[2]),
            (3, &[3]),
            (4, &[2, 2]),
            (5, &[3, 2]),
            (6, &[3, 3]),
            (7, &[3, 2, 2]),
            (8, &[3, 3, 2]),
            (9, &[3, 3, 3]),
            (10, &[3, 3, 2, 2]),
            (11, &[3, 3, 3, 2]),
            (12, &[3, 3, 3, 3]),
        ];
        for (n, want) in table {
            assert_eq!(layout(*n), *want, "n = {n}");
        }
    }

    #[test]
    fn rows_stay_balanced_for_any_n() {
        for n in 1..=100 {
            let rows = layout(n);
            assert_eq!(rows.iter().sum::<usize>(), n, "n = {n}");
            assert_eq!(rows.len(), n.div_ceil(3), "n = {n}");
            let max = *rows.iter().max().unwrap();
            let min = *rows.iter().min().unwrap();
            assert!(max <= 3, "n = {n}: a row wider than 3");
            assert!(max - min <= 1, "n = {n}: rows differ by more than 1");
            assert!(
                rows.windows(2).all(|w| w[0] >= w[1]),
                "n = {n}: fuller rows must come first"
            );
        }
    }

    #[test]
    fn neighbor_walks_pane_order_horizontally() {
        // 5 panes → [3, 2]: 0 1 2 / 3 4
        assert_eq!(neighbor(5, 0, Direction::Right), Some(1));
        assert_eq!(neighbor(5, 1, Direction::Left), Some(0));
        // horizontal moves cross row boundaries
        assert_eq!(neighbor(5, 2, Direction::Right), Some(3));
        assert_eq!(neighbor(5, 3, Direction::Left), Some(2));
    }

    #[test]
    fn neighbor_stops_at_the_edges() {
        assert_eq!(neighbor(5, 0, Direction::Left), None);
        assert_eq!(neighbor(5, 4, Direction::Right), None);
        assert_eq!(neighbor(5, 1, Direction::Up), None);
        assert_eq!(neighbor(5, 4, Direction::Down), None);
        assert_eq!(neighbor(1, 0, Direction::Right), None);
    }

    #[test]
    fn neighbor_keeps_the_column_across_rows() {
        // 6 panes → [3, 3]
        assert_eq!(neighbor(6, 1, Direction::Down), Some(4));
        assert_eq!(neighbor(6, 5, Direction::Up), Some(2));
    }

    #[test]
    fn neighbor_clamps_the_column_to_a_narrower_row() {
        // 5 panes → [3, 2]: rightmost of row 0 lands on rightmost of row 1
        assert_eq!(neighbor(5, 2, Direction::Down), Some(4));
        // 7 panes → [3, 2, 2]
        assert_eq!(neighbor(7, 4, Direction::Up), Some(1));
        assert_eq!(neighbor(7, 2, Direction::Down), Some(4));
    }

    #[test]
    fn neighbor_of_an_out_of_range_index_is_none() {
        assert_eq!(neighbor(3, 7, Direction::Left), None);
        assert_eq!(neighbor(0, 0, Direction::Down), None);
    }

    #[test]
    fn cells_cover_the_unit_square() {
        for n in 1..=20 {
            let cells = cells(n);
            assert_eq!(cells.len(), n, "n = {n}");
            let area: f64 = cells.iter().map(|c| c.width * c.height).sum();
            assert!((area - 1.0).abs() < EPS, "n = {n}: area {area}");
            for c in &cells {
                assert!(c.x >= -EPS && c.x + c.width <= 1.0 + EPS, "n = {n}");
                assert!(c.y >= -EPS && c.y + c.height <= 1.0 + EPS, "n = {n}");
            }
        }
    }

    #[test]
    fn single_pane_fills_the_area_minus_margin() {
        let rect = pixel(cells(1)[0], 116.0, 66.0, 8.0);
        assert!((rect.x - 8.0).abs() < EPS);
        assert!((rect.y - 8.0).abs() < EPS);
        assert!((rect.width - 100.0).abs() < EPS);
        assert!((rect.height - 50.0).abs() < EPS);
    }

    #[test]
    fn neighbours_share_a_full_gap() {
        let cells = cells(2);
        let left = pixel(cells[0], 116.0, 66.0, 8.0);
        let right = pixel(cells[1], 116.0, 66.0, 8.0);
        assert!((left.x - 8.0).abs() < EPS);
        assert!((left.width - 46.0).abs() < EPS, "width {}", left.width);
        assert!((right.x - 62.0).abs() < EPS, "x {}", right.x);
        assert!((right.x - (left.x + left.width) - 8.0).abs() < EPS);
        assert!((right.x + right.width - 108.0).abs() < EPS);
    }

    #[test]
    fn grid_corner_pane_is_inset_on_inner_sides_only() {
        // 4 panes → 2×2; bottom-right pane touches other panes on its
        // top and left, the window edge on its right and bottom
        let rect = pixel(cells(4)[3], 116.0, 116.0, 8.0);
        assert!((rect.x - 62.0).abs() < EPS);
        assert!((rect.y - 62.0).abs() < EPS);
        assert!((rect.width - 46.0).abs() < EPS);
        assert!((rect.height - 46.0).abs() < EPS);
    }

    #[test]
    fn five_panes_split_three_then_two() {
        let cells = cells(5);
        // top row: three panes, a third of the width each
        assert!((cells[0].width - 1.0 / 3.0).abs() < EPS);
        assert!((cells[0].height - 0.5).abs() < EPS);
        // bottom row: two panes, half the width each
        assert!((cells[3].width - 0.5).abs() < EPS);
        assert!((cells[3].y - 0.5).abs() < EPS);
    }
}
