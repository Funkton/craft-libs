//! Room for a drop. While a panel or group is dragged, the docked groups and column bars slide
//! to where the release would put them, so the space the drag lands in opens before the drop,
//! and they slide back when the drag ends without one. Drawing and drop targets stay those of
//! the layout itself: the shapes each group drew this frame are moved to where it is shown.

use std::{fmt::Debug, hash::Hash};

use egui::{Context, LayerId, Rect, epaint::ClippedShape, layers::ShapeIdx};

use super::{Drawn, Renderer};
use crate::docking::{Action, Layout, Node};

/// A docked part that slides as one: a group, or a column's bar, named by its first panel
/// that is not dragged.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Unit<P> {
    Group(P),
    Bar(P),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Slide {
    from: Rect,
    to: Rect,
    start: f64,
}

impl Slide {
    fn at(&self, now: f64, duration: f32) -> Rect {
        let t = if duration > 0.0 { ((now - self.start) as f32 / duration).clamp(0.0, 1.0) } else { 1.0 };
        // Ease out, as the landing outline does.
        self.from.lerp_towards(&self.to, 1.0 - (1.0 - t).powi(3))
    }

    fn done(&self, now: f64, duration: f32) -> bool {
        self.from == self.to || now - self.start >= f64::from(duration)
    }
}

/// The slides of a dock area, kept from frame to frame while any of them moves.
#[derive(Clone, Debug)]
struct Slides<P> {
    /// The panels dragged, which name no unit, while the units make room for them and settle.
    members: Vec<P>,
    units: Vec<(Unit<P>, Slide)>,
}

impl<P> Default for Slides<P> {
    fn default() -> Self {
        Self { members: Vec::new(), units: Vec::new() }
    }
}

/// Where the docked tree drew this frame: its layer and the range of its shapes there.
#[derive(Clone, Copy, Debug)]
pub(in crate::docking) struct Painted {
    pub(in crate::docking) layer: LayerId,
    pub(in crate::docking) start: usize,
    pub(in crate::docking) end: usize,
}

impl Painted {
    pub(in crate::docking) fn next(ctx: &Context, layer: LayerId) -> usize {
        ctx.graphics(|g| g.get(layer).map_or(0, |list| list.next_idx().0))
    }
}

impl<P: Clone + Eq + Hash + Debug + Send + Sync + 'static> Renderer<'_, P> {
    /// This frame's drag would make `action` of `members` on release: make room for it.
    pub(super) fn make_room(&mut self, action: Action<P>, members: &[P]) {
        self.room = Some((action, members.to_vec()));
    }

    /// The docked units of `layout` and their rects, leaving out `members`.
    fn units(&self, layout: &Layout<P>, members: &[P]) -> Vec<(Unit<P>, Rect)> {
        let Some(root) = &layout.root else { return Vec::new() };
        let mut drawn = Vec::new();
        self.sizer().groups(root, self.workspace, &mut drawn);
        let mut out = Vec::new();
        for Drawn { node, rect, .. } in drawn {
            match node {
                Node::Dock { side, columns } => {
                    for (index, _, rect) in self.dock_columns(*side, columns, rect).rects {
                        let Some(column) = columns.get(index) else { continue };
                        let Some(name) = first(column.groups.iter().flat_map(|g| &g.panels), members) else { continue };
                        if column.iconic {
                            out.push((Unit::Bar(name), rect));
                            continue;
                        }
                        let (bar, body, heights) = self.column_layout(column, rect);
                        out.push((Unit::Bar(name), bar));
                        // The column keeps the scroll state of its first panel, as drawn.
                        let overflow = (heights.iter().sum::<f32>() - body.height()).max(0.0);
                        let scroll = column.groups.first().and_then(|g| g.panels.first()).and_then(|p| self.column_scroll.get(p));
                        let area = body.translate(egui::vec2(0.0, -scroll.copied().unwrap_or(0.0).min(overflow)));
                        for (group, rect) in column.groups.iter().zip(super::dock::stacked(area, &heights)) {
                            if let Some(name) = first(&group.panels, members) {
                                out.push((Unit::Group(name), rect.intersect(body)));
                            }
                        }
                    }
                }
                Node::Tabs { panels, .. } => out.extend(first(panels, members).map(|name| (Unit::Group(name), rect))),
                Node::Stack { entries } => out.extend(first(entries.iter().map(|e| &e.panel), members).map(|name| (Unit::Group(name), rect))),
                Node::Split { .. } => {}
            }
        }
        out.retain(|(_, rect)| crate::docking::targets::usable(*rect));
        out
    }

    /// Slides the docked units drawn in `painted` towards where this frame's drag
    /// ([`Self::make_room`]) would put them, or back to their own places once it ends, over
    /// [`crate::docking::DockStyle::preview_animation_time`]. Units the drop would close are
    /// hidden, and so is what lies between units (dividers) while any unit is away.
    pub(in crate::docking) fn slide_units(&mut self, ctx: &Context, painted: Option<Painted>) {
        let key = self.id.with("room-slides");
        let room = self.room.take();
        let duration = self.style.preview_animation_time;
        let stored = ctx.data(|d| d.get_temp::<Slides<P>>(key));
        let (Some(painted), true) = (painted, duration.is_finite() && duration > 0.0) else {
            ctx.data_mut(|d| d.remove::<Slides<P>>(key));
            return;
        };
        if room.is_none() && stored.is_none() {
            return;
        }
        let mut slides = stored.unwrap_or_default();
        if let Some((_, members)) = &room {
            slides.members.clone_from(members);
        }
        let members = slides.members.clone();
        let now = ctx.input(|i| i.time);
        let drawn = self.units(self.layout, &members);
        // On the release the host applies the drop after this frame, which still draws the
        // layout before it: the units keep going to where the drop puts them.
        let actions = match room {
            Some((action, _)) => vec![action],
            None => self.output.actions.clone(),
        };
        let targets = (!actions.is_empty())
            .then(|| {
                let mut candidate = self.layout.clone();
                candidate.apply_all(actions, |p| self.permissions(p)).ok()?;
                Some(self.units(&candidate, &members))
            })
            .flatten();
        let mut shown: Vec<(Rect, Option<Rect>)> = Vec::with_capacity(drawn.len());
        let mut kept = Vec::with_capacity(drawn.len());
        let mut moving = false;
        for (unit, rect) in &drawn {
            let to = match &targets {
                Some(targets) => targets.iter().find(|(u, _)| u == unit).map(|(_, r)| *r),
                None => Some(*rect),
            };
            let Some(to) = to else {
                // Closed by the drop: hidden, and back in place when it returns.
                shown.push((*rect, None));
                continue;
            };
            let previous = slides.units.iter().find(|(u, _)| u == unit).map(|(_, s)| *s);
            let slide = match previous {
                Some(slide) if slide.to == to => slide,
                Some(slide) => Slide { from: slide.at(now, duration), to, start: now },
                None => Slide { from: *rect, to, start: now },
            };
            let at = slide.at(now, duration);
            moving |= !slide.done(now, duration);
            shown.push((*rect, Some(at)));
            if at != *rect || !slide.done(now, duration) {
                kept.push((unit.clone(), slide));
            }
        }
        let away = shown.iter().any(|(rect, at)| *at != Some(*rect));
        if kept.is_empty() && targets.is_none() {
            ctx.data_mut(|d| d.remove::<Slides<P>>(key));
        } else {
            ctx.data_mut(|d| d.insert_temp(key, Slides { members, units: kept }));
        }
        if moving {
            ctx.request_repaint();
        }
        if away {
            relocate(ctx, painted, &shown);
        }
    }
}

/// The first of `panels` that is not one of `members`.
fn first<'a, P: PartialEq + Clone + 'a>(panels: impl IntoIterator<Item = &'a P>, members: &[P]) -> Option<P> {
    panels.into_iter().find(|p| !members.contains(p)).cloned()
}

/// Moves each shape in `painted` from the unit it was drawn in to where that unit is shown,
/// clipped to it; hides the shapes of hidden units and of what lies between units.
fn relocate(ctx: &Context, painted: Painted, shown: &[(Rect, Option<Rect>)]) {
    ctx.graphics_mut(|graphics| {
        let Some(list) = graphics.get_mut(painted.layer) else { return };
        let end = painted.end.min(list.next_idx().0);
        let entries: Vec<ClippedShape> = list.all_entries().skip(painted.start).take(end.saturating_sub(painted.start)).cloned().collect();
        for (offset, ClippedShape { clip_rect, mut shape, .. }) in entries.into_iter().enumerate() {
            let index = ShapeIdx(painted.start + offset);
            let bounds = shape.visual_bounding_rect().intersect(clip_rect);
            if !bounds.is_positive() {
                continue;
            }
            let unit = shown.iter().find(|(rect, _)| rect.expand(0.5).contains_rect(bounds));
            let Some((rect, at)) = unit else {
                // A background behind the units stays; a divider between them goes.
                if !shown.iter().any(|(rect, _)| bounds.contains_rect(*rect)) {
                    list.reset_shape(index);
                }
                continue;
            };
            let Some(at) = at else {
                list.reset_shape(index);
                continue;
            };
            if at == rect {
                continue;
            }
            let delta = at.min - rect.min;
            relocate_shape(&mut shape, *rect, *at);
            let clip = clip_rect.intersect(*rect).translate(delta).intersect(*at);
            if clip.is_positive() {
                list.mutate_shape(index, |entry| {
                    entry.clip_rect = clip;
                    entry.shape = shape;
                });
            } else {
                list.reset_shape(index);
            }
        }
    });
}

/// Moves `shape` from the unit at `from` to the unit at `to`. A rectangle that frames the unit
/// (its fill or border, within a few points of its edges) is stretched to frame `to`, so the
/// unit's frame takes its new size; anything else keeps its size.
fn relocate_shape(shape: &mut egui::Shape, from: Rect, to: Rect) {
    const NEAR: f32 = 4.0;
    match shape {
        egui::Shape::Vec(shapes) => shapes.iter_mut().for_each(|shape| relocate_shape(shape, from, to)),
        egui::Shape::Rect(frame) if (frame.rect.min - from.min).abs().max_elem() <= NEAR && (frame.rect.max - from.max).abs().max_elem() <= NEAR => {
            frame.rect = Rect::from_min_max(to.min + (frame.rect.min - from.min), to.max + (frame.rect.max - from.max));
        }
        shape => shape.translate(to.min - from.min),
    }
}
