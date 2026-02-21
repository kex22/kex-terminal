#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

pub struct Screen {
    total_rows: u16,
    total_cols: u16,
}

const STATUS_BAR_HEIGHT: u16 = 1;

impl Screen {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            total_rows: rows,
            total_cols: cols,
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.total_rows = rows;
        self.total_cols = cols;
    }

    pub fn pane_area(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.total_cols,
            height: self.total_rows.saturating_sub(STATUS_BAR_HEIGHT),
        }
    }

    pub fn status_bar_area(&self) -> Rect {
        let pane_h = self.total_rows.saturating_sub(STATUS_BAR_HEIGHT);
        Rect {
            x: 0,
            y: pane_h,
            width: self.total_cols,
            height: STATUS_BAR_HEIGHT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_cover_full_screen() {
        let s = Screen::new(24, 80);
        let pane = s.pane_area();
        let bar = s.status_bar_area();
        assert_eq!(pane.y, 0);
        assert_eq!(pane.height + bar.height, 24);
        assert_eq!(bar.y, pane.height);
        assert_eq!(pane.width, 80);
        assert_eq!(bar.width, 80);
    }

    #[test]
    fn resize_updates_areas() {
        let mut s = Screen::new(24, 80);
        s.resize(40, 120);
        let pane = s.pane_area();
        let bar = s.status_bar_area();
        assert_eq!(pane.height, 39);
        assert_eq!(bar.y, 39);
        assert_eq!(pane.width, 120);
    }

    #[test]
    fn tiny_screen() {
        let s = Screen::new(1, 10);
        let pane = s.pane_area();
        let bar = s.status_bar_area();
        assert_eq!(pane.height, 0);
        assert_eq!(bar.height, 1);
    }
}
