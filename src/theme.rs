use std::{io, path::PathBuf};

use gpui_component::{Theme, ThemeMode};
use gpui_kit::{App, px, rgb};

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub canvas: u32,
    pub sidebar: u32,
    pub surface: u32,
    pub chrome: u32,
    pub ink: u32,
    pub muted: u32,
    pub border: u32,
    pub grid: u32,
    pub accent: u32,
    pub selection: u32,
    pub hover: u32,
    pub selected_hover: u32,
    pub today_header: u32,
    pub today_column: u32,
    pub today_icon: u32,
    pub completed_icon: u32,
    pub timer: u32,
    pub timer_border: u32,
    pub timer_separator: u32,
    pub timer_idle: u32,
    pub error: u32,
    pub current_time: u32,
}

impl Palette {
    pub fn current(cx: &App) -> Self {
        Self::for_mode(Theme::global(cx).mode)
    }

    fn for_mode(mode: ThemeMode) -> Self {
        if mode.is_dark() {
            Self {
                dark: true,
                canvas: 0x191b23,
                sidebar: 0x1e202a,
                surface: 0x22242f,
                chrome: 0x252733,
                ink: 0xe4e6ef,
                muted: 0xa5acbd,
                border: 0x3a3e50,
                grid: 0x2b2e3b,
                accent: 0xa89ff4,
                selection: 0x36324f,
                hover: 0x2b2e3d,
                selected_hover: 0x423d5e,
                today_header: 0x353246,
                today_column: 0x20222f,
                today_icon: 0x8cb6ff,
                completed_icon: 0x72cdb0,
                timer: 0x2f2b43,
                timer_border: 0x4b4566,
                timer_separator: 0x514966,
                timer_idle: 0xa39ab9,
                error: 0xffa2b4,
                current_time: 0xff818b,
            }
        } else {
            Self {
                dark: false,
                canvas: 0xffffff,
                sidebar: 0xfbfbfd,
                surface: 0xfafafd,
                chrome: 0xf4f5f8,
                ink: 0x252735,
                muted: 0x687080,
                border: 0xe5e7ed,
                grid: 0xeff0f4,
                accent: 0x7770e8,
                selection: 0xecebf8,
                hover: 0xf0f0f5,
                selected_hover: 0xe5e3f4,
                today_header: 0xe8e7f1,
                today_column: 0xf8f8fc,
                today_icon: 0x5286d9,
                completed_icon: 0x249c80,
                timer: 0xf0effa,
                timer_border: 0xe0deef,
                timer_separator: 0xdcd9ed,
                timer_idle: 0xaaa7ba,
                error: 0x9f1239,
                current_time: 0xed6067,
            }
        }
    }

    pub fn task_colors(self, color: &str, completed: bool) -> (u32, u32, u32) {
        if completed {
            return if self.dark {
                (0x272a35, self.muted, 0x484d60)
            } else {
                (0xf2f3f5, self.muted, 0xd7dae1)
            };
        }
        if self.dark {
            match color {
                "blue" => (0x21394b, 0xb1ddf7, 0x416781),
                "green" => (0x203e35, 0xade4ce, 0x426e5d),
                "amber" => (0x43381f, 0xf2d58e, 0x786538),
                "rose" => (0x442b39, 0xf4bfd0, 0x7e4e65),
                _ => (0x33304e, 0xd7d0ff, 0x60578d),
            }
        } else {
            match color {
                "blue" => (0xe1f3fc, 0x21668f, 0x9ed4ee),
                "green" => (0xe0f6ed, 0x226b52, 0x9bdbc1),
                "amber" => (0xfff4cd, 0x815b0d, 0xe7cc77),
                "rose" => (0xffe7ec, 0x9f3452, 0xefb5c4),
                _ => (0xe9e7fc, 0x5146a5, 0xbeb7f0),
            }
        }
    }
}

pub fn init(cx: &mut App) {
    let mode = preference_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .filter(|value| value.trim() == "dark")
        .map(|_| ThemeMode::Dark)
        .unwrap_or(ThemeMode::Light);
    apply(mode, cx);
}

pub fn toggle(cx: &mut App) -> io::Result<()> {
    let mode = if Theme::global(cx).is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    apply(mode, cx);
    let path = preference_path()?;
    std::fs::create_dir_all(
        path.parent()
            .expect("Theme file has an application directory"),
    )?;
    std::fs::write(path, mode.name())
}

fn preference_path() -> io::Result<PathBuf> {
    let directory = dirs::config_dir()
        .ok_or_else(|| io::Error::other("Could not find your configuration folder"))?;
    let app = if cfg!(target_os = "windows") {
        "DayPlanner"
    } else {
        "dayplanner"
    };
    Ok(directory.join(app).join("theme"))
}

fn apply(mode: ThemeMode, cx: &mut App) {
    // GPUI loads the mode's defaults first; apply our overrides afterward.
    Theme::change(mode, None, cx);
    let palette = Palette::for_mode(mode);
    Theme::update(cx, |theme| {
        theme.font_size = px(14.);
        theme.radius = px(6.);
        theme.shadow = false;
        theme.colors.primary = rgb(palette.accent).into();
        theme.colors.ring = rgb(palette.accent).into();
        if palette.dark {
            theme.colors.background = rgb(palette.canvas).into();
            theme.colors.foreground = rgb(palette.ink).into();
            theme.colors.muted_foreground = rgb(palette.muted).into();
            theme.colors.border = rgb(palette.border).into();
            theme.colors.input = rgb(palette.border).into();
            theme.colors.popover = rgb(palette.surface).into();
            theme.colors.popover_foreground = rgb(palette.ink).into();
            theme.colors.accent = rgb(palette.selection).into();
            theme.colors.accent_foreground = rgb(palette.ink).into();
            theme.colors.selection = rgb(palette.selected_hover).into();
            theme.colors.caret = rgb(palette.accent).into();
            theme.colors.primary_foreground = rgb(palette.canvas).into();
            theme.colors.primary_hover = rgb(0xbab2ff).into();
            theme.colors.primary_active = rgb(0x978ce3).into();
            theme.colors.button_primary = theme.colors.primary;
            theme.colors.button_primary_foreground = theme.colors.primary_foreground;
            theme.colors.button_primary_hover = theme.colors.primary_hover;
            theme.colors.button_primary_active = theme.colors.primary_active;
        }
    });
}
