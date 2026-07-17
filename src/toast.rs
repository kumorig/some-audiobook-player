//! Small corner "toast" notifications.
//!
//! `egui-notify` would be the obvious choice, but every published version (and
//! its git branches) still pins egui 0.34, which conflicts with our egui 0.35.
//! This is a minimal stand-in with the same feel: auto-dismissing toasts that
//! stack in the bottom-right corner and fade in/out.

use std::time::{Duration, Instant};

use eframe::egui;

use crate::icons;

const TTL: Duration = Duration::from_millis(4000);
const FADE_IN: f32 = 0.12;
const FADE_OUT: f32 = 0.45;

#[derive(Clone, Copy)]
pub enum Level {
    Success,
    Info,
    Warning,
    Error,
}

impl Level {
    fn color(self) -> egui::Color32 {
        match self {
            Level::Success => egui::Color32::from_rgb(0x46, 0xB4, 0x6E),
            Level::Info => egui::Color32::from_rgb(0x4C, 0x8B, 0xF5),
            Level::Warning => egui::Color32::from_rgb(0xE0, 0xA0, 0x30),
            Level::Error => egui::Color32::from_rgb(0xDA, 0x54, 0x4F),
        }
    }
}

struct Toast {
    text: String,
    level: Level,
    created: Instant,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    fn push(&mut self, level: Level, text: impl Into<String>) {
        self.items.push(Toast {
            text: text.into(),
            level,
            created: Instant::now(),
        });
    }

    pub fn success(&mut self, text: impl Into<String>) {
        self.push(Level::Success, text);
    }
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Level::Info, text);
    }
    #[allow(dead_code)]
    pub fn warning(&mut self, text: impl Into<String>) {
        self.push(Level::Warning, text);
    }
    #[allow(dead_code)]
    pub fn error(&mut self, text: impl Into<String>) {
        self.push(Level::Error, text);
    }

    /// Draw the toasts and expire old ones. Call once per frame.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.items.retain(|t| t.created.elapsed() < TTL);
        if self.items.is_empty() {
            return;
        }
        // Keep animating and expiring even when nothing else requests repaints.
        ctx.request_repaint();

        let mut close: Option<usize> = None;
        egui::Area::new(egui::Id::new("sap_toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-14.0, -14.0))
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for (i, t) in self.items.iter().enumerate() {
                    let age = t.created.elapsed().as_secs_f32();
                    let remaining = TTL.as_secs_f32() - age;
                    let alpha = (age / FADE_IN)
                        .min(1.0)
                        .min((remaining / FADE_OUT).clamp(0.0, 1.0));
                    ui.scope(|ui| {
                        ui.set_opacity(alpha);
                        egui::Frame::popup(ui.style())
                            .stroke(egui::Stroke::new(1.0, t.level.color()))
                            .corner_radius(8.0)
                            .show(ui, |ui| {
                                ui.set_max_width(340.0);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("●").color(t.level.color()));
                                    ui.add(egui::Label::new(&t.text).wrap());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button(icons::X).clicked() {
                                                close = Some(i);
                                            }
                                        },
                                    );
                                });
                            });
                    });
                }
            });
        if let Some(i) = close {
            self.items.remove(i);
        }
    }
}
