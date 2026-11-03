//! Two areas, one above the other or side by side, with a divider between
//! them that drags to resize.

use eframe::egui::{self, Align, CursorIcon, Layout, Rect, Sense, Stroke, Ui, UiBuilder, pos2};

use crate::theme;

/// The space between the two areas; the divider line runs down its middle.
const GAP: f32 = 12.0;
/// Each area keeps at least this much while there is room for both.
const MIN: f32 = 60.0;

/// One of the two areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    /// Above, or left.
    First,
    Second,
}

pub struct Split {
    id: egui::Id,
    /// The first area left of the second instead of above it.
    side_by_side: bool,
    /// The share a double-click on the divider resets to.
    default: f32,
}

impl Split {
    pub fn new(id: egui::Id, side_by_side: bool) -> Self {
        Self {
            id,
            side_by_side,
            default: 0.5,
        }
    }

    pub fn default_share(mut self, share: f32) -> Self {
        self.default = share;
        self
    }

    /// Fills the rest of `ui`, calling `content` once per pane: the first
    /// gets `share` of the room, the second the rest. Returns true when a drag
    /// ended or a double-click reset the share, so the caller can keep it.
    pub fn show(
        self,
        ui: &mut Ui,
        share: &mut f32,
        mut content: impl FnMut(&mut Ui, Pane),
    ) -> bool {
        let rect = ui.available_rect_before_wrap();
        let (start, total) = if self.side_by_side {
            (rect.left(), rect.width())
        } else {
            (rect.top(), rect.height())
        };
        if !share.is_finite() {
            *share = self.default;
        }

        let divider = self.gap_rect(rect, first_size(total, *share, MIN));
        let handle = ui.interact(divider, self.id, Sense::click_and_drag());
        let mut done = false;
        if handle.double_clicked() {
            *share = self.default;
            done = true;
        } else if handle.dragged()
            && let Some(pointer) = handle.interact_pointer_pos()
        {
            let along = if self.side_by_side {
                pointer.x
            } else {
                pointer.y
            };
            *share = share_at(total, along - start, MIN);
        }
        done |= handle.drag_stopped();
        let cursor = if self.side_by_side {
            CursorIcon::ResizeColumn
        } else {
            CursorIcon::ResizeRow
        };
        let handle = handle.on_hover_cursor(cursor);
        let active = handle.hovered() || handle.dragged();
        handle.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, true, "Resize script and results")
        });

        let size = first_size(total, *share, MIN);
        let divider = self.gap_rect(rect, size);
        let (first_rect, second_rect) = if self.side_by_side {
            (
                Rect::from_min_max(rect.min, pos2(divider.left(), rect.bottom())),
                Rect::from_min_max(pos2(divider.right(), rect.top()), rect.max),
            )
        } else {
            (
                Rect::from_min_max(rect.min, pos2(rect.right(), divider.top())),
                Rect::from_min_max(pos2(rect.left(), divider.bottom()), rect.max),
            )
        };
        for (pane, rect) in [(Pane::First, first_rect), (Pane::Second, second_rect)] {
            ui.scope_builder(
                UiBuilder::new()
                    .id_salt(pane)
                    .max_rect(rect)
                    .layout(Layout::top_down(Align::Min)),
                |ui| {
                    // A little slack for focus rings drawn around the content.
                    ui.set_clip_rect(rect.expand(4.0).intersect(ui.clip_rect()));
                    content(ui, pane);
                },
            );
        }

        let palette = theme::palette(ui);
        let stroke = if active {
            Stroke::new(2.0, palette.accent)
        } else {
            Stroke::new(1.0, palette.line)
        };
        if self.side_by_side {
            ui.painter()
                .vline(divider.center().x, divider.y_range(), stroke);
        } else {
            ui.painter()
                .hline(divider.x_range(), divider.center().y, stroke);
        }
        ui.advance_cursor_after_rect(rect);
        done
    }

    /// The gap after a first area of `size`.
    fn gap_rect(&self, rect: Rect, size: f32) -> Rect {
        if self.side_by_side {
            let left = rect.left() + size;
            Rect::from_min_max(pos2(left, rect.top()), pos2(left + GAP, rect.bottom()))
        } else {
            let top = rect.top() + size;
            Rect::from_min_max(pos2(rect.left(), top), pos2(rect.right(), top + GAP))
        }
    }
}

/// The first area's size along the split: `share` of the room the gap
/// leaves, but at least `min` for each area. With no room for both minimums
/// the areas split evenly.
fn first_size(total: f32, share: f32, min: f32) -> f32 {
    let room = (total - GAP).max(0.0);
    if room < 2.0 * min {
        return room / 2.0;
    }
    (room * share.clamp(0.0, 1.0)).clamp(min, room - min)
}

/// The share that puts the divider's middle at `pointer`, measured from the
/// start of the split, kept within the minimums.
fn share_at(total: f32, pointer: f32, min: f32) -> f32 {
    let room = total - GAP;
    if room < 2.0 * min {
        return 0.5;
    }
    (pointer - GAP / 2.0).clamp(min, room - min) / room
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_size_follows_the_share_within_the_minimums() {
        // 1012 tall leaves 1000 of room.
        assert_eq!(first_size(1012.0, 0.4, 60.0), 400.0);
        assert_eq!(first_size(1012.0, 0.01, 60.0), 60.0);
        assert_eq!(first_size(1012.0, 0.99, 60.0), 940.0);
        assert_eq!(first_size(1012.0, 7.0, 60.0), 940.0);
        // Too small for both minimums: split evenly.
        assert_eq!(first_size(112.0, 0.9, 60.0), 50.0);
        assert_eq!(first_size(0.0, 0.5, 60.0), 0.0);
    }

    #[test]
    fn share_at_puts_the_divider_under_the_pointer() {
        assert_eq!(share_at(1012.0, 406.0, 60.0), 0.4);
        assert_eq!(share_at(1012.0, 0.0, 60.0), 0.06);
        assert_eq!(share_at(1012.0, 5000.0, 60.0), 0.94);
        assert_eq!(share_at(100.0, 30.0, 60.0), 0.5);
    }

    #[test]
    fn the_share_keeps_its_proportion_when_the_window_grows() {
        let share = share_at(512.0, 206.0, 60.0);
        assert_eq!(first_size(512.0, share, 60.0), 200.0);
        assert_eq!(first_size(1012.0, share, 60.0), 400.0);
    }
}
