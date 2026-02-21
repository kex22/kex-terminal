use super::input::Direction;
use super::screen::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug)]
pub(super) enum LayoutNode {
    Leaf {
        terminal_id: String,
    },
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

pub struct PaneLayout {
    root: LayoutNode,
    focused: String,
}

impl PaneLayout {
    pub fn new(terminal_id: String) -> Self {
        let focused = terminal_id.clone();
        Self {
            root: LayoutNode::Leaf { terminal_id },
            focused,
        }
    }

    pub fn focused_terminal(&self) -> &str {
        &self.focused
    }

    pub fn compute_rects(&self, area: Rect) -> Vec<(String, Rect)> {
        let mut result = Vec::new();
        Self::collect_rects(&self.root, area, &mut result);
        result
    }

    fn collect_rects(node: &LayoutNode, area: Rect, out: &mut Vec<(String, Rect)>) {
        match node {
            LayoutNode::Leaf { terminal_id } => out.push((terminal_id.clone(), area)),
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let (a, b) = Self::split_rect(area, *direction, *ratio);
                Self::collect_rects(first, a, out);
                Self::collect_rects(second, b, out);
            }
        }
    }

    fn split_rect(area: Rect, dir: SplitDirection, ratio: f32) -> (Rect, Rect) {
        match dir {
            SplitDirection::Vertical => {
                let w1 = (area.width as f32 * ratio) as u16;
                let w2 = area.width.saturating_sub(w1);
                (
                    Rect {
                        x: area.x,
                        y: area.y,
                        width: w1,
                        height: area.height,
                    },
                    Rect {
                        x: area.x + w1,
                        y: area.y,
                        width: w2,
                        height: area.height,
                    },
                )
            }
            SplitDirection::Horizontal => {
                let h1 = (area.height as f32 * ratio) as u16;
                let h2 = area.height.saturating_sub(h1);
                (
                    Rect {
                        x: area.x,
                        y: area.y,
                        width: area.width,
                        height: h1,
                    },
                    Rect {
                        x: area.x,
                        y: area.y + h1,
                        width: area.width,
                        height: h2,
                    },
                )
            }
        }
    }

    pub fn split(&mut self, direction: SplitDirection, new_terminal_id: String) {
        if Self::split_node(&mut self.root, &self.focused, direction, &new_terminal_id) {
            self.focused = new_terminal_id;
        }
    }

    fn split_node(
        node: &mut LayoutNode,
        target: &str,
        direction: SplitDirection,
        new_id: &str,
    ) -> bool {
        match node {
            LayoutNode::Leaf { terminal_id } if terminal_id == target => {
                let old = std::mem::replace(
                    node,
                    LayoutNode::Leaf {
                        terminal_id: String::new(),
                    },
                );
                *node = LayoutNode::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(old),
                    second: Box::new(LayoutNode::Leaf {
                        terminal_id: new_id.to_string(),
                    }),
                };
                true
            }
            LayoutNode::Split { first, second, .. } => {
                Self::split_node(first, target, direction, new_id)
                    || Self::split_node(second, target, direction, new_id)
            }
            _ => false,
        }
    }

    pub fn close_focused(&mut self) -> Option<String> {
        let closed = self.focused.clone();
        if let Some(sibling_id) = Self::remove_node(&mut self.root, &self.focused) {
            self.focused = sibling_id;
            Some(closed)
        } else {
            None
        }
    }

    fn remove_node(node: &mut LayoutNode, target: &str) -> Option<String> {
        match node {
            LayoutNode::Split { first, second, .. } => {
                if matches!(first.as_ref(), LayoutNode::Leaf { terminal_id } if terminal_id == target)
                {
                    let sibling = std::mem::replace(
                        second.as_mut(),
                        LayoutNode::Leaf {
                            terminal_id: String::new(),
                        },
                    );
                    let sibling_id = Self::first_leaf_id(&sibling);
                    *node = sibling;
                    return Some(sibling_id);
                }
                if matches!(second.as_ref(), LayoutNode::Leaf { terminal_id } if terminal_id == target)
                {
                    let sibling = std::mem::replace(
                        first.as_mut(),
                        LayoutNode::Leaf {
                            terminal_id: String::new(),
                        },
                    );
                    let sibling_id = Self::first_leaf_id(&sibling);
                    *node = sibling;
                    return Some(sibling_id);
                }
                Self::remove_node(first, target).or_else(|| Self::remove_node(second, target))
            }
            _ => None,
        }
    }

    fn first_leaf_id(node: &LayoutNode) -> String {
        match node {
            LayoutNode::Leaf { terminal_id } => terminal_id.clone(),
            LayoutNode::Split { first, .. } => Self::first_leaf_id(first),
        }
    }

    pub fn resize_focused(&mut self, direction: Direction, delta: f32) {
        let axis = match direction {
            Direction::Left | Direction::Right => SplitDirection::Vertical,
            Direction::Up | Direction::Down => SplitDirection::Horizontal,
        };
        let grow_first = matches!(direction, Direction::Right | Direction::Down);
        Self::resize_node(&mut self.root, &self.focused, axis, delta, grow_first);
    }

    fn resize_node(
        node: &mut LayoutNode,
        target: &str,
        axis: SplitDirection,
        delta: f32,
        grow_first: bool,
    ) -> bool {
        match node {
            LayoutNode::Leaf { terminal_id } => terminal_id == target,
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let in_first = Self::resize_node(first, target, axis, delta, grow_first);
                let in_second = if !in_first {
                    Self::resize_node(second, target, axis, delta, grow_first)
                } else {
                    false
                };
                if (in_first || in_second) && *direction == axis {
                    let adjust = if (in_first && grow_first) || (in_second && !grow_first) {
                        delta
                    } else {
                        -delta
                    };
                    *ratio = (*ratio + adjust).clamp(0.1, 0.9);
                }
                in_first || in_second
            }
        }
    }

    pub fn navigate(&mut self, direction: Direction, area: Rect) {
        let rects = self.compute_rects(area);
        let Some(focused_rect) = rects
            .iter()
            .find(|(id, _)| id == &self.focused)
            .map(|(_, r)| *r)
        else {
            return;
        };
        let candidate = rects
            .iter()
            .filter(|(id, _)| id != &self.focused)
            .filter(|(_, r)| match direction {
                Direction::Left => {
                    r.x + r.width <= focused_rect.x
                        && ranges_overlap(r.y, r.height, focused_rect.y, focused_rect.height)
                }
                Direction::Right => {
                    r.x >= focused_rect.x + focused_rect.width
                        && ranges_overlap(r.y, r.height, focused_rect.y, focused_rect.height)
                }
                Direction::Up => {
                    r.y + r.height <= focused_rect.y
                        && ranges_overlap(r.x, r.width, focused_rect.x, focused_rect.width)
                }
                Direction::Down => {
                    r.y >= focused_rect.y + focused_rect.height
                        && ranges_overlap(r.x, r.width, focused_rect.x, focused_rect.width)
                }
            })
            .min_by_key(|(_, r)| match direction {
                Direction::Left => focused_rect.x as i32 - (r.x + r.width) as i32,
                Direction::Right => r.x as i32 - (focused_rect.x + focused_rect.width) as i32,
                Direction::Up => focused_rect.y as i32 - (r.y + r.height) as i32,
                Direction::Down => r.y as i32 - (focused_rect.y + focused_rect.height) as i32,
            });
        if let Some((id, _)) = candidate {
            self.focused = id.clone();
        }
    }
}

fn ranges_overlap(a_start: u16, a_len: u16, b_start: u16, b_len: u16) -> bool {
    a_start < b_start + b_len && b_start < a_start + a_len
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        }
    }

    #[test]
    fn single_pane_covers_full_area() {
        let layout = PaneLayout::new("t1".into());
        let rects = layout.compute_rects(area());
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].0, "t1");
        assert_eq!(rects[0].1, area());
    }

    #[test]
    fn vertical_split_creates_left_right() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Vertical, "t2".into());
        let rects = layout.compute_rects(area());
        assert_eq!(rects.len(), 2);
        // First pane: left half
        assert_eq!(rects[0].1.x, 0);
        assert_eq!(rects[0].1.width, 40);
        // Second pane: right half
        assert_eq!(rects[1].1.x, 40);
        assert_eq!(rects[1].1.width, 40);
    }

    #[test]
    fn horizontal_split_creates_top_bottom() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Horizontal, "t2".into());
        let rects = layout.compute_rects(area());
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].1.y, 0);
        assert_eq!(rects[0].1.height, 12);
        assert_eq!(rects[1].1.y, 12);
        assert_eq!(rects[1].1.height, 12);
    }

    #[test]
    fn split_focuses_new_pane() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Vertical, "t2".into());
        assert_eq!(layout.focused_terminal(), "t2");
    }

    #[test]
    fn nested_split_three_panes() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Vertical, "t2".into());
        // t2 is focused, split it horizontally
        layout.split(SplitDirection::Horizontal, "t3".into());
        let rects = layout.compute_rects(area());
        assert_eq!(rects.len(), 3);
    }

    #[test]
    fn close_returns_to_sibling() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Vertical, "t2".into());
        let closed = layout.close_focused();
        assert_eq!(closed, Some("t2".into()));
        assert_eq!(layout.focused_terminal(), "t1");
        assert_eq!(layout.compute_rects(area()).len(), 1);
    }

    #[test]
    fn close_last_pane_returns_none() {
        let mut layout = PaneLayout::new("t1".into());
        assert_eq!(layout.close_focused(), None);
    }

    #[test]
    fn navigate_left_right() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Vertical, "t2".into());
        assert_eq!(layout.focused_terminal(), "t2");
        layout.navigate(Direction::Left, area());
        assert_eq!(layout.focused_terminal(), "t1");
        layout.navigate(Direction::Right, area());
        assert_eq!(layout.focused_terminal(), "t2");
    }

    #[test]
    fn navigate_at_boundary_stays() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Vertical, "t2".into());
        // t2 is rightmost, navigate right should stay
        layout.navigate(Direction::Right, area());
        assert_eq!(layout.focused_terminal(), "t2");
    }

    #[test]
    fn navigate_up_down() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Horizontal, "t2".into());
        assert_eq!(layout.focused_terminal(), "t2");
        layout.navigate(Direction::Up, area());
        assert_eq!(layout.focused_terminal(), "t1");
        layout.navigate(Direction::Down, area());
        assert_eq!(layout.focused_terminal(), "t2");
    }

    #[test]
    fn resize_changes_ratio() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Vertical, "t2".into());
        // t2 is second child (right), resize Right should grow t2 (shrink t1)
        layout.resize_focused(Direction::Right, 0.1);
        let rects = layout.compute_rects(area());
        assert!(rects[0].1.width < 40, "t1 should shrink");
        assert!(rects[1].1.width > 40, "t2 should grow");
    }

    #[test]
    fn resize_clamps_ratio() {
        let mut layout = PaneLayout::new("t1".into());
        layout.split(SplitDirection::Vertical, "t2".into());
        // Try to resize way beyond bounds
        for _ in 0..20 {
            layout.resize_focused(Direction::Left, 0.1);
        }
        let rects = layout.compute_rects(area());
        // First pane should not be smaller than 10% of 80 = 8
        assert!(rects[0].1.width >= 8);
    }
}
