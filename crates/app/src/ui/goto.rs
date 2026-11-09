//! Ctrl+K: go to any collection by typing part of its name. Recent
//! collections come first; while typing, ties go to the database of the
//! active tab, then its connection, then open connections, then the most
//! recently used database and connection.

use std::collections::HashMap;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Modifiers,
    Rect, RichText, Role, Sense, Stroke, TextEdit, WidgetInfo, pos2, vec2,
};
use nordpil_backend::CollectionKind;
use nordpil_backend::navigation::Recent;
use uuid::Uuid;

use crate::connections::Selection;
use crate::icons;
use crate::theme;
use crate::widgets::kit;

/// Recent collections shown before anything is typed.
const RECENT_SHOWN: usize = 8;
const MAX_RESULTS: usize = 50;
const ROW_HEIGHT: f32 = 44.0;

/// A collection that can be opened.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub target: Selection,
    pub connection_name: String,
    pub color: Color32,
    pub kind: CollectionKind,
    /// Its connection is open.
    pub connected: bool,
    /// It has a tab already.
    pub open: bool,
}

pub enum Outcome {
    Open,
    Close,
    Pick(Selection),
}

pub struct GotoPalette {
    query: String,
    selected: usize,
    focus_requested: bool,
}

/// How well a candidate matches; lower is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Exact,
    Prefix,
    Substring,
    Fuzzy,
    /// Matches the database or connection, not the collection name.
    Elsewhere,
}

/// A ranked candidate and the matched characters of its name.
#[derive(Debug)]
pub struct Match {
    pub index: usize,
    pub highlight: Vec<usize>,
}

impl GotoPalette {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            focus_requested: false,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        candidates: &[Candidate],
        recents: &[Recent],
        current: Option<&Selection>,
        searches_closed: bool,
    ) -> Outcome {
        let matches = rank(&self.query, candidates, recents, current);
        let (down, up, enter) = ctx.input_mut(|input| {
            (
                input.count_and_consume_key(Modifiers::NONE, Key::ArrowDown),
                input.count_and_consume_key(Modifiers::NONE, Key::ArrowUp),
                input.consume_key(Modifiers::NONE, Key::Enter),
            )
        });
        if !matches.is_empty() {
            let len = matches.len();
            self.selected = (self.selected.min(len - 1) + down + len - up % len) % len;
        }
        let mut outcome = Outcome::Open;
        if enter && let Some(found) = matches.get(self.selected) {
            outcome = Outcome::Pick(candidates[found.index].target.clone());
        }

        let visuals = ctx.global_style().visuals.clone();
        let palette = theme::of(ctx, visuals.dark_mode);
        let area = egui::Modal::default_area(egui::Id::unique("goto_palette"))
            .anchor(Align2::CENTER_TOP, vec2(0.0, 8.0));
        let modal = kit::modal(ctx, egui::Id::unique("goto_palette"))
            .area(area)
            .frame(
                Frame::new()
                    .fill(palette.surface)
                    .stroke(Stroke::new(1.0, palette.line_strong))
                    .corner_radius(CornerRadius::same(14))
                    .shadow(visuals.window_shadow),
            )
            .show(ctx, |ui| {
                ui.set_width(640.0_f32.min(ctx.content_rect().width() - 32.0));
                ui.spacing_mut().item_spacing.y = 0.0;
                self.input(ui);
                ui.painter().hline(
                    ui.min_rect().x_range(),
                    ui.min_rect().bottom(),
                    Stroke::new(1.0, palette.line),
                );
                if let Some(picked) = self.results(ui, candidates, &matches) {
                    outcome = Outcome::Pick(picked);
                }
                footer(ui, searches_closed);
            });
        if matches!(outcome, Outcome::Open) && modal.should_close() {
            outcome = Outcome::Close;
        }
        outcome
    }

    fn input(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(ui);
        Frame::new()
            .inner_margin(Margin {
                left: 18,
                right: 14,
                top: 0,
                bottom: 0,
            })
            .show(ui, |ui| {
                ui.set_height(54.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.label(
                        RichText::new(icons::SEARCH)
                            .size(18.0)
                            .color(palette.subtle),
                    );
                    ui.add_space(4.0);
                    let width = (ui.available_width() - 48.0).max(80.0);
                    {
                        let before = self.query.clone();
                        let response = ui.add(
                            TextEdit::singleline(&mut self.query)
                                .frame(Frame::new())
                                .hint_text("Go to collection…")
                                .font(FontId::proportional(16.0))
                                .align(Align2::LEFT_CENTER)
                                .min_size(vec2(0.0, 34.0))
                                .desired_width(width),
                        );
                        response.widget_info(|| {
                            WidgetInfo::labeled(Role::TextInput, true, "Go to collection")
                        });
                        if !self.focus_requested {
                            response.request_focus();
                            self.focus_requested = true;
                        }
                        if self.query != before {
                            self.selected = 0;
                        }
                    }
                    kit::keys(ui, &["Esc"]);
                });
            });
    }

    fn results(
        &mut self,
        ui: &mut egui::Ui,
        candidates: &[Candidate],
        matches: &[Match],
    ) -> Option<Selection> {
        let palette = theme::palette(ui);
        let mut picked = None;
        Frame::new().inner_margin(Margin::same(6)).show(ui, |ui| {
            if matches.is_empty() {
                ui.add_space(14.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(if candidates.is_empty() {
                            "No collections yet. Connect to a server to list its collections."
                        } else {
                            "No collection matches."
                        })
                        .color(palette.subtle),
                    );
                });
                ui.add_space(14.0);
                return;
            }
            if self.query.trim().is_empty() {
                Frame::new()
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        kit::section_label(ui, "Recent");
                    });
            }
            egui::ScrollArea::vertical()
                .max_height(ROW_HEIGHT * 8.0)
                .show(ui, |ui| {
                    for (row, found) in matches.iter().enumerate() {
                        let candidate = &candidates[found.index];
                        let selected = row == self.selected;
                        let response = result_row(ui, candidate, &found.highlight, selected);
                        if selected && response.rect.is_positive() {
                            response.scroll_to_me(None);
                        }
                        if response.hovered() && ui.input(|i| i.pointer.is_moving()) {
                            self.selected = row;
                        }
                        if response.clicked() {
                            picked = Some(candidate.target.clone());
                        }
                    }
                });
        });
        picked
    }
}

fn result_row(
    ui: &mut egui::Ui,
    candidate: &Candidate,
    highlight: &[usize],
    selected: bool,
) -> egui::Response {
    let palette = theme::palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, CornerRadius::same(9), palette.accent_soft);
    }
    let y = rect.center().y;
    let icon = match candidate.kind {
        CollectionKind::View => icons::EYE,
        CollectionKind::Timeseries => icons::CLOCK,
        CollectionKind::Collection => icons::LAYERS,
    };
    painter.text(
        pos2(rect.left() + 14.0, y),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(17.0),
        if selected {
            palette.accent
        } else {
            palette.subtle
        },
    );

    // Right side, laid out from the right edge.
    let mut right = rect.right() - 12.0;
    if selected {
        painter.text(
            pos2(right, y),
            Align2::RIGHT_CENTER,
            icons::CORNER_DOWN_LEFT,
            FontId::proportional(14.0),
            palette.subtle,
        );
    }
    right -= 28.0;
    let place = format!(
        "{} · {}",
        candidate.connection_name, candidate.target.database
    );
    let place_galley = painter.layout_no_wrap(place, FontId::proportional(13.0), palette.muted);
    let place_left = right - place_galley.size().x;
    painter.galley(
        pos2(place_left, y - place_galley.size().y / 2.0),
        place_galley,
        palette.muted,
    );
    painter.circle_filled(pos2(place_left - 11.0, y), 4.0, candidate.color);
    right = place_left - 26.0;

    let badge = if !candidate.connected {
        Some(format!("{}  connects first", icons::PLUG))
    } else if candidate.open {
        Some("open".to_owned())
    } else {
        None
    };
    if let Some(badge) = badge {
        let galley = painter.layout_no_wrap(badge, FontId::proportional(12.0), palette.subtle);
        let badge_rect = Rect::from_min_size(
            pos2(right - galley.size().x - 14.0, y - 10.0),
            vec2(galley.size().x + 14.0, 20.0),
        );
        if candidate.connected {
            painter.rect(
                badge_rect,
                CornerRadius::same(255),
                palette.raised,
                Stroke::new(1.0, palette.line),
                egui::StrokeKind::Inside,
            );
        }
        painter.galley(
            pos2(badge_rect.left() + 7.0, y - galley.size().y / 2.0),
            galley,
            palette.subtle,
        );
        right = badge_rect.left() - 10.0;
    }

    let name = &candidate.target.collection;
    let mut job = LayoutJob::default();
    for (index, c) in name.chars().enumerate() {
        let hit = highlight.contains(&index);
        let font = if hit {
            FontId::new(14.5, theme::semibold())
        } else {
            FontId::proportional(14.5)
        };
        let color = if hit {
            palette.accent_text
        } else {
            palette.text
        };
        job.append(&c.to_string(), 0.0, TextFormat::simple(font, color));
    }
    job.wrap.max_width = (right - rect.left() - 42.0).max(40.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    painter.galley(
        pos2(rect.left() + 42.0, y - galley.size().y / 2.0),
        galley,
        palette.text,
    );
    let label = format!(
        "{name} in {} on {}",
        candidate.target.database, candidate.connection_name
    );
    response.widget_info(|| WidgetInfo::selected(Role::ListBoxOption, true, selected, &label));
    response
}

fn footer(ui: &mut egui::Ui, searches_closed: bool) {
    let palette = theme::palette(ui);
    let rect = ui.available_rect_before_wrap();
    ui.painter()
        .hline(rect.x_range(), rect.top(), Stroke::new(1.0, palette.line));
    Frame::new()
        .fill(palette.main)
        .inner_margin(Margin::symmetric(16, 0))
        .corner_radius(CornerRadius {
            nw: 0,
            ne: 0,
            sw: 14,
            se: 14,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(42.0);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let hint = |ui: &mut egui::Ui, keys: &[&str], text: &str| {
                    kit::keys(ui, keys);
                    ui.label(RichText::new(text).size(12.5).color(palette.subtle));
                    ui.add_space(10.0);
                };
                hint(ui, &["↑", "↓"], "move");
                hint(ui, &["↵"], "open");
                hint(ui, &["Esc"], "close");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(if searches_closed {
                            "Searching all saved connections"
                        } else {
                            "Searching open connections"
                        })
                        .size(12.5)
                        .color(palette.subtle),
                    );
                });
            });
        });
}

/// Candidates matching `query`, best first. With an empty query, the recent
/// collections, newest first. `current` is the collection of the active tab.
pub fn rank(
    query: &str,
    candidates: &[Candidate],
    recents: &[Recent],
    current: Option<&Selection>,
) -> Vec<Match> {
    let key = |target: &Selection| {
        (
            target.connection,
            target.database.clone(),
            target.collection.clone(),
        )
    };
    let mut collection_rank: HashMap<(Uuid, String, String), usize> = HashMap::new();
    let mut database_rank: HashMap<(Uuid, String), usize> = HashMap::new();
    let mut connection_rank: HashMap<Uuid, usize> = HashMap::new();
    for (position, recent) in recents.iter().enumerate() {
        collection_rank
            .entry((
                recent.connection,
                recent.database.clone(),
                recent.collection.clone(),
            ))
            .or_insert(position);
        database_rank
            .entry((recent.connection, recent.database.clone()))
            .or_insert(position);
        connection_rank.entry(recent.connection).or_insert(position);
    }
    // 0 in the active tab's database, 1 on its connection, 2 elsewhere.
    let closeness = |target: &Selection| match current {
        Some(current) if current.connection != target.connection => 2,
        Some(current) if current.database != target.database => 1,
        Some(_) => 0,
        None => 2,
    };

    let query = query.trim().to_lowercase();
    if query.is_empty() {
        let mut recent: Vec<(usize, usize)> = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                collection_rank
                    .get(&key(&candidate.target))
                    .map(|&rank| (rank, index))
            })
            .collect();
        recent.sort();
        return recent
            .into_iter()
            .take(RECENT_SHOWN)
            .map(|(_, index)| Match {
                index,
                highlight: Vec::new(),
            })
            .collect();
    }

    let mut scored: Vec<_> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let (tier, highlight) = match_name(&query, candidate)?;
            let target = &candidate.target;
            let sort_key = (
                tier,
                closeness(target),
                !candidate.connected,
                database_rank
                    .get(&(target.connection, target.database.clone()))
                    .copied()
                    .unwrap_or(usize::MAX),
                connection_rank
                    .get(&target.connection)
                    .copied()
                    .unwrap_or(usize::MAX),
                collection_rank
                    .get(&key(target))
                    .copied()
                    .unwrap_or(usize::MAX),
                target.collection.len(),
                target.collection.to_lowercase(),
            );
            Some((sort_key, Match { index, highlight }))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0));
    scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, found)| found)
        .collect()
}

/// How `query` (lowercase) matches the candidate, with the matched char
/// indices of the collection name.
fn match_name(query: &str, candidate: &Candidate) -> Option<(Tier, Vec<usize>)> {
    let name = candidate.target.collection.to_lowercase();
    let span = |start: usize| (start..start + query.chars().count()).collect::<Vec<_>>();
    if name == query {
        return Some((Tier::Exact, span(0)));
    }
    if name.starts_with(query) {
        return Some((Tier::Prefix, span(0)));
    }
    if let Some(byte) = name.find(query) {
        return Some((Tier::Substring, span(name[..byte].chars().count())));
    }
    if let Some(positions) = subsequence(query, &name) {
        return Some((Tier::Fuzzy, positions));
    }
    let place = format!(
        "{} {} {}",
        candidate.connection_name.to_lowercase(),
        candidate.target.database.to_lowercase(),
        name
    );
    let dotted = format!("{}.{name}", candidate.target.database.to_lowercase());
    (place.contains(query) || dotted.contains(query) || subsequence(query, &dotted).is_some())
        .then(|| (Tier::Elsewhere, Vec::new()))
}

/// The char indices of `text` that spell `query` in order, if they do.
fn subsequence(query: &str, text: &str) -> Option<Vec<usize>> {
    let mut wanted = query.chars().filter(|c| !c.is_whitespace()).peekable();
    let mut positions = Vec::new();
    for (index, c) in text.chars().enumerate() {
        if wanted.peek() == Some(&c) {
            positions.push(index);
            wanted.next();
        }
    }
    wanted.peek().is_none().then_some(positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(connection: Uuid, name: &str, database: &str, collection: &str) -> Candidate {
        Candidate {
            target: Selection {
                connection,
                database: database.to_owned(),
                collection: collection.to_owned(),
            },
            connection_name: name.to_owned(),
            color: Color32::WHITE,
            kind: CollectionKind::Collection,
            connected: true,
            open: false,
        }
    }

    fn recent(connection: Uuid, database: &str, collection: &str) -> Recent {
        Recent {
            connection,
            database: database.to_owned(),
            collection: collection.to_owned(),
            opened_at: 0,
        }
    }

    fn names(query: &str, candidates: &[Candidate], recents: &[Recent]) -> Vec<String> {
        names_from(query, candidates, recents, None)
    }

    fn names_from(
        query: &str,
        candidates: &[Candidate],
        recents: &[Recent],
        current: Option<&Selection>,
    ) -> Vec<String> {
        rank(query, candidates, recents, current)
            .into_iter()
            .map(|found| {
                let target = &candidates[found.index].target;
                format!(
                    "{}/{}.{}",
                    candidates[found.index].connection_name, target.database, target.collection
                )
            })
            .collect()
    }

    #[test]
    fn ranks_by_match_then_recent_database_then_connection() {
        let (local, production) = (Uuid::new_v4(), Uuid::new_v4());
        let candidates = [
            candidate(production, "Production", "archive", "records"),
            candidate(production, "Production", "analytics", "order_events"),
            candidate(production, "Production", "shop", "orders"),
            candidate(local, "Local", "shop", "orders_by_day"),
            candidate(local, "Local", "shop", "orders"),
            candidate(local, "Local", "shop", "customers"),
        ];
        let recents = [
            recent(local, "shop", "customers"),
            recent(production, "shop", "orders"),
        ];

        assert_eq!(
            names("ord", &candidates, &recents),
            [
                "Local/shop.orders",
                "Local/shop.orders_by_day",
                "Production/shop.orders",
                "Production/analytics.order_events",
                "Production/archive.records",
            ]
        );
    }

    #[test]
    fn ranks_active_database_then_its_connection_then_open_connections() {
        let (local, staging, production) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let closed = Candidate {
            connected: false,
            ..candidate(local, "Local", "shop", "orders")
        };
        let candidates = [
            closed,
            candidate(production, "Production", "analytics", "old_orders"),
            candidate(staging, "Staging", "shop", "orders"),
            candidate(production, "Production", "shop", "orders"),
            candidate(production, "Production", "analytics", "orders"),
        ];
        let recents = [recent(local, "shop", "orders")];
        let current = candidates[1].target.clone();

        assert_eq!(
            names_from("orders", &candidates, &recents, Some(&current)),
            [
                "Production/analytics.orders",
                "Production/shop.orders",
                "Staging/shop.orders",
                "Local/shop.orders",
                "Production/analytics.old_orders",
            ]
        );
    }

    #[test]
    fn empty_query_lists_recents_newest_first() {
        let local = Uuid::new_v4();
        let candidates = [
            candidate(local, "Local", "shop", "orders"),
            candidate(local, "Local", "shop", "customers"),
            candidate(local, "Local", "shop", "never_opened"),
        ];
        let recents = [
            recent(local, "shop", "customers"),
            recent(local, "shop", "orders"),
        ];

        assert_eq!(
            names("", &candidates, &recents),
            ["Local/shop.customers", "Local/shop.orders"]
        );
    }

    #[test]
    fn matches_fuzzy_and_by_database() {
        let local = Uuid::new_v4();
        let candidates = [
            candidate(local, "Local", "shop", "customers"),
            candidate(local, "Local", "logs", "events"),
        ];

        assert_eq!(names("cstm", &candidates, &[]), ["Local/shop.customers"]);
        assert_eq!(names("logs", &candidates, &[]), ["Local/logs.events"]);
        let found = rank("cstm", &candidates, &[], None);
        assert_eq!(found[0].highlight, [0, 2, 3, 5]);
    }
}
