//! Desktop surfaces use the kit's semantic colors, including its focus and disabled states.
use gpui_kit::component::Theme;
use gpui_kit::{px, rgb, App};

pub const SURFACE_RADIUS: f32 = 12.0;
pub const CONTROL_RADIUS: f32 = 8.0;

pub fn apply(cx: &mut App) {
    Theme::update(cx, |theme| {
        let dark = theme.is_dark();
        let color = |light, dark_color| rgb(if dark { dark_color } else { light }).into();
        theme.radius = px(CONTROL_RADIUS);
        theme.radius_lg = px(SURFACE_RADIUS);
        let c = &mut theme.colors;
        c.background = color(0xf8fafc, 0x101722);
        c.foreground = color(0x17243b, 0xe5edf8);
        c.sidebar = color(0xf0f4f9, 0x151e2d);
        c.sidebar_foreground = c.foreground;
        c.border = color(0xdce4ef, 0x2b384c);
        c.sidebar_border = c.border;
        c.muted = color(0xeef2f8, 0x202d40);
        c.muted_foreground = color(0x52647d, 0xa4b4cb);
        c.accent = color(0xe2eaff, 0x263a60);
        c.accent_foreground = c.foreground;
        c.primary = color(0x3459d6, 0x3d61cc);
        c.primary_hover = color(0x294bbf, 0x4668d7);
        c.primary_active = color(0x2340a5, 0x345bd8);
        c.primary_foreground = rgb(0xffffff).into();
        c.button_primary = c.primary;
        c.button_primary_hover = c.primary_hover;
        c.button_primary_active = c.primary_active;
        c.button_primary_foreground = c.primary_foreground;
        c.ring = color(0x3459d6, 0x91adff);
        c.input = c.border;
        c.popover = c.sidebar;
        c.popover_foreground = c.foreground;
        c.list_hover = c.muted;
        c.list_active = c.accent;
        c.list_active_border = c.primary;
        c.selection = c.primary.alpha(0.25);
    });
}
