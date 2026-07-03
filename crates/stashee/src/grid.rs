//! Animated tiling container. Children are allocated straight from
//! `stashee_core::layout` cells; on every layout change surviving
//! panes slide from their current spot to their new cell (ease-out,
//! 180 ms) and appearing panes fade in in place. With system
//! animations disabled the reflow is instant (AdwAnimation follows
//! the enable-animations setting).

use std::cell::{Cell, RefCell};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use stashee_core::layout;

const GAP: f64 = 8.0;
const REFLOW_MS: u32 = 180;

mod imp {
    use super::*;

    /// One managed pane: where the running animation started (`from`;
    /// `None` = new pane, fades in) and where the last allocation put
    /// it (`last` — the live position, mid-animation included, so a
    /// retarget starts from what is actually on screen).
    pub struct Child {
        pub widget: gtk::Widget,
        pub from: Option<layout::Cell>,
        /// Opacity when the running animation started. Anything below
        /// 1.0 keeps fading toward 1.0: a pane spawned mid-animation is
        /// retargeted as a survivor and must not stay half-transparent.
        pub fade_from: f64,
        pub last: layout::Cell,
    }

    #[derive(Default)]
    pub struct TilingGrid {
        pub children: RefCell<Vec<Child>>,
        /// Animation progress, `0.0..=1.0`; `1.0` = settled.
        pub progress: Cell<f64>,
        pub animation: RefCell<Option<adw::TimedAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TilingGrid {
        const NAME: &'static str = "StasheeTilingGrid";
        type Type = super::TilingGrid;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TilingGrid {
        fn constructed(&self) {
            self.parent_constructed();
            self.progress.set(1.0);
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
        }

        fn dispose(&self) {
            if let Some(animation) = self.animation.take() {
                animation.pause();
            }
            for child in self.children.take() {
                child.widget.unparent();
            }
        }
    }

    impl WidgetImpl for TilingGrid {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            // children must be measured before they can be allocated;
            // the grid itself takes whatever space it is given
            for child in self.children.borrow().iter() {
                let _ = child.widget.measure(orientation, for_size);
            }
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let progress = self.progress.get();
            let mut children = self.children.borrow_mut();
            let cells = layout::cells(children.len());
            for (child, cell) in children.iter_mut().zip(cells) {
                let target = layout::pixel(cell, f64::from(width), f64::from(height), GAP);
                let rect = match child.from {
                    Some(from) if progress < 1.0 => lerp(from, target, progress),
                    _ => target,
                };
                child.last = rect;
                #[allow(clippy::cast_possible_truncation)]
                let allocation = gtk::Allocation::new(
                    rect.x.round() as i32,
                    rect.y.round() as i32,
                    rect.width.round() as i32,
                    rect.height.round() as i32,
                );
                child.widget.size_allocate(&allocation, -1);
                if child.fade_from < 1.0 {
                    child
                        .widget
                        .set_opacity(child.fade_from + (1.0 - child.fade_from) * progress);
                }
            }
        }

        fn unrealize(&self) {
            // realization only drops on teardown; settle so a leaked
            // frame cannot freeze mid-animation
            if let Some(animation) = self.animation.take() {
                animation.pause();
            }
            self.progress.set(1.0);
            self.parent_unrealize();
        }
    }

    fn lerp(from: layout::Cell, to: layout::Cell, t: f64) -> layout::Cell {
        layout::Cell {
            x: from.x + (to.x - from.x) * t,
            y: from.y + (to.y - from.y) * t,
            width: from.width + (to.width - from.width) * t,
            height: from.height + (to.height - from.height) * t,
        }
    }
}

glib::wrapper! {
    pub struct TilingGrid(ObjectSubclass<imp::TilingGrid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for TilingGrid {
    fn default() -> Self {
        Self::new()
    }
}

impl TilingGrid {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Show exactly `panes` (row-major, matching `layout::cells`).
    /// Surviving panes animate from where they are now; new panes fade
    /// in at their cell; removed panes are unparented immediately.
    pub fn set_panes(&self, panes: &[gtk::Widget]) {
        let imp = self.imp();
        // stop without snapping: `last` already holds the on-screen
        // rects, so the new animation starts from them
        if let Some(animation) = imp.animation.take() {
            animation.pause();
        }

        let old = imp.children.take();
        let mut next: Vec<imp::Child> = Vec::with_capacity(panes.len());
        for pane in panes {
            let from = old
                .iter()
                .find(|child| &child.widget == pane)
                .map(|child| child.last);
            let fade_from = if from.is_none() {
                pane.set_parent(self);
                pane.set_opacity(0.0);
                0.0
            } else {
                pane.opacity()
            };
            next.push(imp::Child {
                widget: pane.clone(),
                from,
                fade_from,
                last: layout::Cell {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                },
            });
        }
        for child in old {
            if !panes.contains(&child.widget) {
                child.widget.unparent();
            }
        }
        *imp.children.borrow_mut() = next;

        imp.progress.set(0.0);
        let weak = self.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |value| {
            if let Some(grid) = weak.upgrade() {
                grid.imp().progress.set(value);
                grid.queue_allocate();
            }
        });
        let animation = adw::TimedAnimation::new(self, 0.0, 1.0, REFLOW_MS, target);
        animation.set_easing(adw::Easing::EaseOutCubic);
        // the animation holds a ref to the grid; dropping it when done
        // keeps that cycle bounded to the 180 ms it is needed
        let weak = self.downgrade();
        animation.connect_done(move |_| {
            if let Some(grid) = weak.upgrade() {
                grid.imp().animation.take();
            }
        });
        // store before play(): with animations disabled play() finishes
        // synchronously and done must find the slot to clear
        *imp.animation.borrow_mut() = Some(animation.clone());
        animation.play();
        self.queue_allocate();
    }
}
