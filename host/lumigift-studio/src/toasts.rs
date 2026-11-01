//! Bottom-right toasts (docs/UI_DESIGN.md §6.1): slide up 8 px + fade,
//! info/success auto-dismiss after 4 s (paused while hovered), errors persist.

use std::time::{Duration, Instant};

use egui::{Align2, CornerRadius, Id, Order, Sense, Stroke, StrokeKind, pos2, vec2};

use crate::icons;
use crate::theme::{self, *};
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

pub struct Toast {
    id: u64,
    kind: ToastKind,
    title: String,
    body: Option<String>,
    action: Option<String>,
    born: Instant,
    shown_for: Duration,
    last_tick: Instant,
}

#[derive(Default)]
pub struct Toasts {
    list: Vec<Toast>,
    next: u64,
}

impl Toasts {
    pub fn push(&mut self, kind: ToastKind, title: impl Into<String>, body: Option<String>) -> u64 {
        self.push_action(kind, title, body, None)
    }

    /// A toast with one action button; `show` returns its id when clicked.
    pub fn push_action(
        &mut self,
        kind: ToastKind,
        title: impl Into<String>,
        body: Option<String>,
        action: Option<&str>,
    ) -> u64 {
        self.next += 1;
        let now = Instant::now();
        self.list.push(Toast {
            id: self.next,
            kind,
            title: title.into(),
            body,
            action: action.map(str::to_owned),
            born: now,
            shown_for: Duration::ZERO,
            last_tick: now,
        });
        while self.list.len() > 3 {
            // Drop the oldest non-error first.
            if let Some(i) = self.list.iter().position(|t| t.kind != ToastKind::Error) {
                self.list.remove(i);
            } else {
                self.list.remove(0);
            }
        }
        self.next
    }

    pub fn info(&mut self, t: impl Into<String>) {
        self.push(ToastKind::Info, t, None);
    }
    pub fn success(&mut self, t: impl Into<String>) {
        self.push(ToastKind::Success, t, None);
    }
    pub fn error(&mut self, t: impl Into<String>, body: impl Into<String>) {
        self.push(ToastKind::Error, t, Some(body.into()));
    }

    /// Paint; returns the id of a toast whose action was clicked.
    pub fn show(&mut self, ctx: &egui::Context, bottom_margin: f32) -> Option<u64> {
        let mut clicked = None;
        let screen = ctx.content_rect();
        let mut y = screen.bottom() - 12.0 - bottom_margin;
        let now = Instant::now();
        let mut remove = Vec::new();
        for t in self.list.iter_mut().rev() {
            let age = now.duration_since(t.born).as_secs_f32();
            let appear = egui::emath::easing::cubic_out((age / MOTION_BASE).min(1.0));
            let w = 340.0;
            let h = if t.body.is_some() { 64.0 } else { 44.0 };
            let rect = egui::Rect::from_min_size(
                pos2(screen.right() - 12.0 - w, y - h + (1.0 - appear) * 8.0),
                vec2(w, h),
            );
            y -= h + 8.0;
            let area = egui::Area::new(Id::unique(("toast", t.id)))
                .order(Order::Tooltip)
                .fixed_pos(rect.min)
                .interactable(true);
            let mut hovered = false;
            area.show(ctx, |ui| {
                ui.set_opacity(appear);
                let (r, resp) = ui.allocate_exact_size(rect.size(), Sense::hover());
                hovered = resp.hovered();
                let p = ui.painter();
                let sh = theme::shadow_popover();
                p.add(sh.as_shape(r, CornerRadius::same(R_LG)));
                p.rect_filled(r, CornerRadius::same(R_LG), SURFACE_3);
                p.rect_stroke(r, CornerRadius::same(R_LG), Stroke::new(1.0, BORDER), StrokeKind::Inside);
                widgets::top_highlight(p, r, R_LG as f32);
                let (icon, col) = match t.kind {
                    ToastKind::Info => (icons::INFO, INFO),
                    ToastKind::Success => (icons::CHECK_CIRCLE, OK),
                    ToastKind::Warn => (icons::WARNING, WARN),
                    ToastKind::Error => (icons::WARNING_CIRCLE, ERROR),
                };
                widgets::paint_icon(p, pos2(r.left() + 22.0, r.top() + 22.0), icon, 18.0, col);
                p.text(
                    pos2(r.left() + 40.0, r.top() + 22.0),
                    Align2::LEFT_CENTER,
                    &t.title,
                    theme::font("button"),
                    TEXT,
                );
                if let Some(b) = &t.body {
                    let max_w = r.width() - 52.0 - if t.action.is_some() { 90.0 } else { 0.0 };
                    let b = widgets::truncate(ui, b, &theme::font("small"), max_w);
                    p.text(pos2(r.left() + 40.0, r.top() + 42.0), Align2::LEFT_CENTER, b, theme::font("small"), TEXT_MUTED);
                }
                let mut right = r.right() - 10.0;
                // Dismiss ×
                let xr = egui::Rect::from_center_size(pos2(right - 12.0, r.top() + 22.0), vec2(24.0, 24.0));
                let xresp = ui.interact(xr, Id::unique(("toast-x", t.id)), Sense::click());
                widgets::paint_icon(ui.painter(), xr.center(), icons::X, 14.0, if xresp.hovered() { TEXT } else { TEXT_FAINT });
                if xresp.clicked() {
                    remove.push(t.id);
                }
                right -= 30.0;
                if let Some(a) = &t.action {
                    let font = theme::font("button");
                    let aw = widgets::text_width(ui, a, font.clone()) + 20.0;
                    let ar = egui::Rect::from_min_size(pos2(right - aw, r.top() + 10.0), vec2(aw, 24.0));
                    let aresp = ui.interact(ar, Id::unique(("toast-a", t.id)), Sense::click());
                    let fill = if aresp.hovered() { HOVER } else { ACTIVE };
                    ui.painter().rect_filled(ar, CornerRadius::same(R_SM), fill);
                    ui.painter().text(ar.center(), Align2::CENTER_CENTER, a, font, ACCENT_TEXT);
                    if aresp.clicked() {
                        clicked = Some(t.id);
                        remove.push(t.id);
                    }
                }
            });
            if !hovered {
                t.shown_for += now.duration_since(t.last_tick);
            }
            t.last_tick = now;
            let ttl = if t.action.is_some() { 8.0 } else { 4.0 };
            if t.kind != ToastKind::Error && t.shown_for.as_secs_f32() > ttl {
                remove.push(t.id);
            }
        }
        self.list.retain(|t| !remove.contains(&t.id));
        if !self.list.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        clicked
    }
}
