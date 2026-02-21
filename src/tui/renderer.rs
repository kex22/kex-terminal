use std::io::Write;

use crossterm::{QueueableCommand, cursor::MoveTo, style::Print};

use super::screen::Rect;
use super::vterm::VirtualTerminal;
use crate::error::Result;

pub struct Renderer<W: Write> {
    writer: W,
}

impl<W: Write> Renderer<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub fn render_vterm(&mut self, vterm: &VirtualTerminal, area: &Rect) -> Result<()> {
        let screen = vterm.screen();
        for row in 0..area.height {
            self.writer.queue(MoveTo(area.x, area.y + row))?;
            let mut line = String::new();
            for col in 0..area.width {
                if let Some(cell) = screen.cell(row, col) {
                    let c = cell.contents();
                    if c.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(c);
                    }
                } else {
                    line.push(' ');
                }
            }
            self.writer.queue(Print(&line))?;
        }
        Ok(())
    }

    pub fn render_vterm_rows(
        &mut self,
        vterm: &VirtualTerminal,
        area: &Rect,
        rows: &[u16],
    ) -> Result<()> {
        let screen = vterm.screen();
        for &row in rows {
            if row >= area.height {
                continue;
            }
            self.writer.queue(MoveTo(area.x, area.y + row))?;
            let mut line = String::new();
            for col in 0..area.width {
                if let Some(cell) = screen.cell(row, col) {
                    let c = cell.contents();
                    if c.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(c);
                    }
                } else {
                    line.push(' ');
                }
            }
            self.writer.queue(Print(&line))?;
        }
        Ok(())
    }

    pub fn render_status_bar(&mut self, text: &str, area: &Rect) -> Result<()> {
        use crossterm::style::{Attribute, SetAttribute};
        self.writer.queue(MoveTo(area.x, area.y))?;
        self.writer.queue(SetAttribute(Attribute::Reverse))?;
        let padded: String = if text.chars().count() >= area.width as usize {
            text.chars().take(area.width as usize).collect()
        } else {
            format!("{:<width$}", text, width = area.width as usize)
        };
        self.writer.queue(Print(&padded))?;
        self.writer.queue(SetAttribute(Attribute::Reset))?;
        Ok(())
    }

    /// Draw vertical separator line at column `x` for `height` rows starting at `y`.
    pub fn render_vsep(&mut self, x: u16, y: u16, height: u16) -> Result<()> {
        for row in y..y + height {
            self.writer.queue(MoveTo(x, row))?;
            self.writer.queue(Print("│"))?;
        }
        Ok(())
    }

    /// Draw horizontal separator line at row `y` for `width` columns starting at `x`.
    pub fn render_hsep(&mut self, x: u16, y: u16, width: u16) -> Result<()> {
        self.writer.queue(MoveTo(x, y))?;
        let line: String = "─".repeat(width as usize);
        self.writer.queue(Print(&line))?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_vterm_outputs_content() {
        let mut vt = VirtualTerminal::new(4, 10);
        vt.process(b"hello");
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 4,
        };
        let mut buf = Vec::new();
        let mut r = Renderer::new(&mut buf);
        r.render_vterm(&vt, &area).unwrap();
        r.flush().unwrap();
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("hello"));
    }

    #[test]
    fn render_status_bar_pads() {
        let area = Rect {
            x: 0,
            y: 23,
            width: 20,
            height: 1,
        };
        let mut buf = Vec::new();
        let mut r = Renderer::new(&mut buf);
        r.render_status_bar("test", &area).unwrap();
        r.flush().unwrap();
        let output = String::from_utf8_lossy(&buf);
        assert!(output.contains("test"));
    }
}
