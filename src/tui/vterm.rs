pub struct VirtualTerminal {
    parser: vt100::Parser,
    prev_row_hashes: Vec<u64>,
}

impl VirtualTerminal {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 0),
            prev_row_hashes: vec![0; rows as usize],
        }
    }

    pub fn process(&mut self, data: &[u8]) {
        self.parser.process(data);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        self.prev_row_hashes = vec![0; rows as usize];
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    /// Returns indices of rows that changed since last call to this method.
    pub fn take_dirty_rows(&mut self) -> Vec<u16> {
        use std::hash::{Hash, Hasher};
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        debug_assert_eq!(self.prev_row_hashes.len(), rows as usize);
        let mut dirty = Vec::new();
        for r in 0..rows {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for c in 0..cols {
                if let Some(cell) = screen.cell(r, c) {
                    cell.contents().hash(&mut hasher);
                }
            }
            let h = hasher.finish();
            if r as usize >= self.prev_row_hashes.len() || self.prev_row_hashes[r as usize] != h {
                dirty.push(r);
            }
            if (r as usize) < self.prev_row_hashes.len() {
                self.prev_row_hashes[r as usize] = h;
            }
        }
        dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_plain_text() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"hello");
        let cell = vt.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "h");
        let cell = vt.screen().cell(0, 4).unwrap();
        assert_eq!(cell.contents(), "o");
    }

    #[test]
    fn cursor_moves_after_write() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"abc");
        assert_eq!(vt.cursor_position(), (0, 3));
    }

    #[test]
    fn resize_updates_dimensions() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.resize(10, 40);
        vt.process(b"x");
        assert_eq!(vt.screen().cell(0, 0).unwrap().contents(), "x");
    }

    #[test]
    fn ansi_color_parsed() {
        let mut vt = VirtualTerminal::new(24, 80);
        vt.process(b"\x1b[31mred\x1b[0m");
        let cell = vt.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "r");
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
    }

    #[test]
    fn dirty_rows_tracks_changes() {
        let mut vt = VirtualTerminal::new(4, 10);
        // First call: all rows dirty (initial hash mismatch)
        let dirty = vt.take_dirty_rows();
        assert!(!dirty.is_empty());

        // No changes: no dirty rows
        let dirty = vt.take_dirty_rows();
        assert!(dirty.is_empty());

        // Write to row 0
        vt.process(b"hello");
        let dirty = vt.take_dirty_rows();
        assert!(dirty.contains(&0));
        // Row 1+ should not be dirty
        assert!(!dirty.contains(&2));
    }
}
