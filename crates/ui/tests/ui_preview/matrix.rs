//! Pure matrix and contact-sheet layout calculations, independent of GPUI and Metal.

#[derive(Debug, PartialEq, Eq)]
pub struct Cell<'a> {
    pub screen: &'a str,
    pub style: &'a str,
    pub theme: &'a str,
    pub label: String,
}

pub fn matrix<'a>(screens: &[&'a str], styles: &[&'a str], themes: &[&'a str]) -> Vec<Cell<'a>> {
    let mut cells = Vec::new();
    for &screen in screens {
        for &style in styles {
            for &theme in themes {
                cells.push(Cell { screen, style, theme, label: format!("{style} · {theme}") });
            }
        }
    }
    cells
}

/// Fit equally sized cells into a bounded sheet while preserving their aspect ratio.
/// Returns None if even one pixel per row or column cannot fit.
pub fn cell_size(width: u32, height: u32, columns: u32, rows: u32, limit: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || columns == 0 || rows == 0 || limit == 0 {
        return None;
    }
    let max_width = limit / columns;
    let max_height = limit / rows;
    if max_width == 0 || max_height == 0 { return None; }
    let (w, h) = if u64::from(max_width) * u64::from(height) <= u64::from(max_height) * u64::from(width) {
        (width.min(max_width), (u64::from(height) * u64::from(width.min(max_width)) / u64::from(width)) as u32)
    } else {
        ((u64::from(width) * u64::from(height.min(max_height)) / u64::from(height)) as u32, height.min(max_height))
    };
    if w == 0 || h == 0 { None } else { Some((w, h)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_orders_screens_then_style_rows_then_theme_columns() {
        let cells = matrix(&["chat", "details"], &["minimal", "industrial"], &["light", "dark"]);
        assert_eq!(cells.len(), 8);
        assert_eq!(cells[0].label, "minimal · light");
        assert_eq!((cells[3].screen, cells[3].style, cells[3].theme), ("chat", "industrial", "dark"));
        assert_eq!(cells[4].screen, "details");
        assert!(matrix(&["chat"], &[], &["dark"]).is_empty());
    }

    #[test]
    fn sheet_cells_fit_without_upscaling() {
        assert_eq!(cell_size(1440, 900, 2, 4, 4096), Some((1440, 900)));
        assert_eq!(cell_size(1440, 900, 4, 5, 4096), Some((1024, 640)));
        assert_eq!(cell_size(1440, 900, 1, 0, 4096), None);
        assert_eq!(cell_size(1440, 900, 4097, 1, 4096), None);
    }
}
