//! Feedback while a tab is dragged: its ghost under the pointer, the landing outline moving
//! into place, and the × on closable tabs.

use std::collections::HashMap;

use super::testing::DockDriver;
use super::*;
use crate::layout::{SplitAxis, SplitSize};
use egui::{Pos2, Rect, pos2, vec2};

#[derive(Default)]
struct Bodies {
    bodies: HashMap<u32, Rect>,
    rules: HashMap<u32, Permissions>,
}

impl DockContent<u32> for Bodies {
    fn body(&mut self, ui: &mut egui::Ui, panel: &u32) {
        self.bodies.insert(*panel, ui.max_rect());
    }
    fn permissions(&self, panel: &u32) -> Permissions {
        self.rules.get(panel).copied().unwrap_or_default()
    }
}

type Driver = DockDriver<u32, Bodies>;

/// Frames `[1]` and `[2, 3]` side by side, each half of the area.
fn frames() -> Layout<u32> {
    Layout {
        root: Some(Node::Split {
            axis: SplitAxis::Horizontal,
            size: SplitSize::Ratio(0.5),
            first: Box::new(Node::Tabs { panels: vec![1], active: 0 }),
            second: Box::new(Node::Tabs { panels: vec![2, 3], active: 1 }),
        }),
        floating: Vec::new(),
    }
}

fn driver(style: impl Fn(&mut DockStyle) + 'static) -> Driver {
    DockDriver::new(frames(), Bodies::default()).with_style(style)
}

fn tab(d: &Driver, panel: u32) -> Rect {
    d.hit(&Hit::Tab(panel)).unwrap_or_else(|| panic!("tab {panel}"))
}

/// The group of `panel`: its body and the 26 pt strip above it.
fn group(d: &Driver, panel: u32) -> Rect {
    let body = d.content.bodies.get(&panel).copied().unwrap_or_else(|| panic!("panel {panel} not drawn"));
    Rect::from_min_max(pos2(body.left(), body.top() - 26.0), body.max)
}

/// Filled rects of `size` in the last frame.
fn filled_of_size(d: &Driver, size: egui::Vec2) -> Vec<Rect> {
    d.shapes()
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Rect(shape) if shape.fill != egui::Color32::TRANSPARENT && (shape.rect.size() - size).length() < 0.01 => Some(shape.rect),
            _ => None,
        })
        .collect()
}

/// The landing outlines of the last frame: unfilled rects stroked 2 pt in the accent.
fn outlines(d: &Driver) -> Vec<Rect> {
    let accent = DockStyle::from_visuals(&d.ctx.global_style().visuals).accent;
    d.shapes()
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Rect(shape) if shape.fill == egui::Color32::TRANSPARENT && shape.stroke.width == 2.0 && shape.stroke.color == accent => {
                Some(shape.rect)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_dragged_tab_has_a_ghost_that_follows_the_pointer() {
    let mut d = driver(|style| style.drag_ghost = true);
    let source = tab(&d, 3);
    let to = group(&d, 1).center();
    d.hold(source.center(), to);
    // The tab's own size, under the pointer.
    let ghosts: Vec<Rect> = filled_of_size(&d, source.size()).into_iter().filter(|rect| rect.contains(to)).collect();
    let [ghost] = ghosts[..] else { panic!("one ghost under the pointer, not {ghosts:?}") };
    // It keeps its place under the pointer as the pointer moves.
    let step = vec2(-30.0, 40.0);
    d.move_to(to + step);
    assert!(filled_of_size(&d, source.size()).contains(&ghost.translate(step)), "the ghost moved with the pointer");
    // Released, it is gone.
    d.release(to + step);
    assert!(filled_of_size(&d, source.size()).iter().all(|rect| !rect.contains(to + step)));

    // Off by default.
    let mut d = driver(|_| {});
    let source = tab(&d, 3);
    d.hold(source.center(), to);
    assert!(filled_of_size(&d, source.size()).iter().all(|rect| !rect.contains(to)), "no ghost without drag_ghost");
}

#[test]
fn the_landing_outline_moves_into_place() {
    let time = 0.2;
    let mut d = driver(move |style| {
        style.drag_ghost = true;
        style.preview_animation_time = time;
    });
    let source = tab(&d, 3);
    let left = group(&d, 1);
    let target = left.shrink(1.0);
    d.hold(source.center(), left.center());
    // It starts at the ghost, not at the target.
    assert!(!d.outlined(target), "in place on its first frame");
    let first = outlines(&d);
    // Frames are 1/60 s apart: by `time` it has arrived and stays.
    let mut previous = first.clone();
    let mut moving = 0;
    for _ in 0..((time * 60.0) as usize + 2) {
        d.move_to(left.center());
        let now = outlines(&d);
        if now != previous {
            moving += 1;
        }
        previous = now;
    }
    assert!(moving > 3, "it moved over several frames, not {moving}");
    assert!(d.outlined(target), "in place after the animation");
    // A new target, the top of that group, starts from where the outline was: on the frame the
    // target changes it is still there, and a frame later it has barely left it, nowhere near
    // the ghost.
    let top = pos2(left.center().x, left.top() + 26.0 + 20.0);
    d.move_to(top);
    assert_eq!(outlines(&d), vec![target]);
    d.move_to(top);
    let [moved] = outlines(&d)[..] else { panic!("one outline, not {:?}", outlines(&d)) };
    assert_ne!(moved, target);
    assert!((moved.min - target.min).length() + (moved.max - target.max).length() < (moved.min - source.min).length());
    for _ in 0..((time * 60.0) as usize + 2) {
        d.move_to(top);
    }
    let settled = outlines(&d);
    d.move_to(top);
    assert_eq!(outlines(&d), settled, "settled");
    assert!(settled.len() == 1 && settled[0] != target && left.contains_rect(settled[0]), "{settled:?}");

    // Without an animation time it is in place at once.
    let mut d = driver(|style| style.drag_ghost = true);
    let source = tab(&d, 3);
    d.hold(source.center(), left.center());
    assert!(d.outlined(target));
}

#[test]
fn a_closable_tab_shows_a_close_button_that_closes_it() {
    let rules = HashMap::from([(2, Permissions { closable: false, ..Permissions::default() })]);
    let mut d = DockDriver::new(frames(), Bodies { rules: rules.clone(), ..Bodies::default() }).with_style(|style| style.tab_close_buttons = true);
    let plain = DockDriver::new(frames(), Bodies { rules, ..Bodies::default() });
    let size = DockStyle::default().tab_close_size;
    // The closable tab grows by the × and half the padding; the other keeps its width.
    let widen = size + surface::TAB_PADDING * 0.5;
    assert!((tab(&d, 3).width() - tab(&plain, 3).width() - widen).abs() < 0.01);
    assert_eq!(tab(&d, 2).width(), tab(&plain, 2).width());
    assert!(d.hit(&Hit::TabClose(2)).is_none(), "no × on a tab that may not close");
    let close = d.hit(&Hit::TabClose(3)).expect("× on the closable tab");
    assert!(tab(&d, 3).contains_rect(close));
    assert!((close.width() - size).abs() < 0.01 && (close.height() - size).abs() < 0.01);
    // A click on it closes the panel, and does not also activate it.
    let out = d.click(close.center());
    assert_eq!(out.actions, vec![Action::Close { panel: 3 }]);
    assert!(!d.layout.panels().contains(&&3));
    // A click on the rest of the tab still activates.
    let mut d = DockDriver::new(frames(), Bodies::default()).with_style(|style| style.tab_close_buttons = true);
    let out = d.click(tab(&d, 2).left_center() + vec2(6.0, 0.0));
    assert!(out.actions.contains(&Action::Activate { panel: 2 }));
    // Off by default.
    assert!(plain.hit(&Hit::TabClose(3)).is_none());
}

#[test]
fn a_tab_dragged_by_its_close_button_still_moves() {
    let mut d = driver(|style| style.tab_close_buttons = true);
    let close: Pos2 = d.hit(&Hit::TabClose(3)).expect("×").center();
    let left = group(&d, 1);
    let out = d.drag(close, left.center());
    assert!(out.actions.iter().any(|a| matches!(a, Action::Move { panel: 3, anchor: 1, .. })), "{:?}", out.actions);
    assert!(!out.actions.iter().any(|a| matches!(a, Action::Close { .. })));
}
