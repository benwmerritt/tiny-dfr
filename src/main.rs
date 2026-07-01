use anyhow::{anyhow, Result};
use cairo::{Antialias, Context, Format, ImageSurface, Surface};
use chrono::{
    format::{Item as ChronoItem, StrftimeItems},
    Local, Locale, Timelike,
};
use drm::control::ClipRect;
use freedesktop_icons::lookup;
use input::{
    event::{
        device::DeviceEvent,
        keyboard::{KeyState, KeyboardEvent, KeyboardEventTrait},
        touch::{TouchEvent, TouchEventPosition, TouchEventSlot},
        Event, EventTrait,
    },
    Device as InputDevice, Libinput, LibinputInterface,
};
use input_linux::{uinput::UInputHandle, EventKind, Key, SynchronizeKind};
use input_linux_sys::{input_event, input_id, timeval, uinput_setup};
use libc::{c_char, O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY};
use librsvg_rebind::{prelude::HandleExt, Handle, Rectangle};
use nix::{
    errno::Errno,
    sys::{
        epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags},
        signal::{SigSet, Signal},
    },
};
use privdrop::PrivDrop;
use std::{
    cmp::min,
    collections::HashMap,
    fs::{self, File, OpenOptions},
    os::{
        fd::{AsFd, AsRawFd},
        unix::{fs::OpenOptionsExt, io::OwnedFd},
    },
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use udev::MonitorBuilder;

mod backlight;
mod config;
mod display;
mod fonts;
mod layout;
mod pixel_shift;

use crate::config::ConfigManager;
use backlight::BacklightManager;
use config::{ButtonConfig, Config, WorkspacesCfg};
use display::DrmBackend;
use layout::{
    button_spans, controls_region, hit_index, strip_region, ButtonSpan, LayoutSpec, RegionGeometry,
    CONTROL_SPACING_PX as BUTTON_SPACING_PX, STRIP_SPACING_PX,
};
use pixel_shift::{PixelShiftManager, PIXEL_SHIFT_WIDTH_PX};

const BUTTON_COLOR_INACTIVE: f64 = 0.200;
const BUTTON_COLOR_ACTIVE: f64 = 0.400;
const DEFAULT_ICON_SIZE: i32 = 48;
const TIMEOUT_MS: i32 = 10 * 1000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BatteryState {
    NotCharging,
    Charging,
    Low,
}

struct BatteryImages {
    plain: Vec<Handle>,
    charging: Vec<Handle>,
    bolt: Handle,
}

#[derive(Eq, PartialEq, Copy, Clone)]
enum BatteryIconMode {
    Percentage,
    Icon,
    Both,
}

impl BatteryIconMode {
    fn should_draw_icon(self) -> bool {
        self != BatteryIconMode::Percentage
    }
    fn should_draw_text(self) -> bool {
        self != BatteryIconMode::Icon
    }
}

enum ButtonImage {
    Text(String),
    Svg(Handle),
    Bitmap(ImageSurface),
    Time(Vec<ChronoItem<'static>>, Locale),
    Battery(String, BatteryIconMode, BatteryImages),
    Spacer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ButtonAction {
    Keys(Vec<Key>),
    OpenOverlay(String),
    CloseOverlay,
    None,
}

impl ButtonAction {
    fn from_config(cfg: &ButtonConfig) -> ButtonAction {
        if let Some(name) = &cfg.open_overlay {
            ButtonAction::OpenOverlay(name.clone())
        } else if cfg.close_overlay.unwrap_or(false) {
            ButtonAction::CloseOverlay
        } else if cfg.action.is_empty() {
            ButtonAction::None
        } else {
            ButtonAction::Keys(cfg.action.clone())
        }
    }

    fn keys(&self) -> &[Key] {
        match self {
            ButtonAction::Keys(keys) => keys,
            ButtonAction::OpenOverlay(_) | ButtonAction::CloseOverlay | ButtonAction::None => &[],
        }
    }
}

struct Button {
    // Used by the upcoming control socket path to address buttons from
    // external state without coupling tiny-dfr to Niri-specific concepts.
    #[allow(dead_code)]
    id: Option<String>,
    image: ButtonImage,
    changed: bool,
    pressed: bool,
    highlighted: bool,
    action: ButtonAction,
    icon_width: f64,
    icon_height: f64,
}

fn try_load_svg(path: &str) -> Result<ButtonImage> {
    Ok(ButtonImage::Svg(
        Handle::from_file(path).map_err(|_| anyhow!("failed to load image"))?,
    ))
}

fn try_load_png(path: impl AsRef<Path>, icon_width: i32, icon_height: i32) -> Result<ButtonImage> {
    let mut file = File::open(path)?;
    let surf = ImageSurface::create_from_png(&mut file)?;
    if surf.height() == icon_height && surf.width() == icon_width {
        return Ok(ButtonImage::Bitmap(surf));
    }
    let resized = ImageSurface::create(Format::ARgb32, icon_width, icon_height).unwrap();
    let c = Context::new(&resized).unwrap();
    c.scale(
        icon_width as f64 / surf.width() as f64,
        icon_height as f64 / surf.height() as f64,
    );
    c.set_source_surface(surf, 0.0, 0.0).unwrap();
    c.set_antialias(Antialias::Best);
    c.paint().unwrap();
    Ok(ButtonImage::Bitmap(resized))
}

fn try_load_image(
    name: impl AsRef<str>,
    theme: Option<impl AsRef<str>>,
    icon_width: i32,
    icon_height: i32,
) -> Result<ButtonImage> {
    let name = name.as_ref();
    let locations;

    // Load list of candidate locations
    if let Some(theme) = theme {
        // Freedesktop icons
        let theme = theme.as_ref();
        let candidates = vec![
            lookup(name)
                .with_cache()
                .with_theme(theme)
                .with_size(icon_height as u16)
                .force_svg()
                .find(),
            lookup(name)
                .with_cache()
                .with_theme(theme)
                .force_svg()
                .find(),
        ];

        // .flatten() removes `None` and unwraps `Some` values
        locations = candidates.into_iter().flatten().collect();
    } else {
        // Standard file icons
        locations = vec![
            PathBuf::from(format!("/etc/tiny-dfr/{name}.svg")),
            PathBuf::from(format!("/etc/tiny-dfr/{name}.png")),
            PathBuf::from(format!("/usr/share/tiny-dfr/{name}.svg")),
            PathBuf::from(format!("/usr/share/tiny-dfr/{name}.png")),
        ];
    };

    // Try to load each candidate
    let mut last_err = anyhow!("no suitable icon path was found"); // in case locations is empty

    for location in locations {
        let result = match location.extension().and_then(|s| s.to_str()) {
            Some("png") => try_load_png(&location, icon_width, icon_height),
            Some("svg") => try_load_svg(
                location
                    .to_str()
                    .ok_or(anyhow!("image path is not unicode"))?,
            ),
            _ => Err(anyhow!("invalid file extension")),
        };

        match result {
            Ok(image) => return Ok(image),
            Err(err) => {
                last_err = err.context(format!("while loading path {}", location.display()));
            }
        };
    }

    // if function hasn't returned by now, all sources have been exhausted
    Err(last_err.context(format!("failed loading all possible paths for icon {name}")))
}

fn find_battery_device() -> Option<String> {
    let power_supply_path = "/sys/class/power_supply";
    if let Ok(entries) = fs::read_dir(power_supply_path) {
        for entry in entries.flatten() {
            let dev_path = entry.path();
            let type_path = dev_path.join("type");
            if let Ok(typ) = fs::read_to_string(&type_path) {
                if typ.trim() == "Battery" {
                    if let Some(name) = dev_path.file_name().and_then(|n| n.to_str()) {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

fn get_battery_state(battery: &str) -> (u32, BatteryState) {
    let status_path = format!("/sys/class/power_supply/{}/status", battery);
    let status = fs::read_to_string(&status_path).unwrap_or_else(|_| "Unknown".to_string());

    let capacity = {
        #[cfg(target_arch = "x86_64")]
        {
            let charge_now_path = format!("/sys/class/power_supply/{}/charge_now", battery);
            let charge_full_path = format!("/sys/class/power_supply/{}/charge_full", battery);
            let charge_now = fs::read_to_string(&charge_now_path)
                .ok()
                .and_then(|s| s.trim().parse::<f64>().ok());
            let charge_full = fs::read_to_string(&charge_full_path)
                .ok()
                .and_then(|s| s.trim().parse::<f64>().ok());
            match (charge_now, charge_full) {
                (Some(now), Some(full)) if full > 0.0 => ((now / full) * 100.0).round() as u32,
                _ => 100,
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let capacity_path = format!("/sys/class/power_supply/{}/capacity", battery);
            fs::read_to_string(&capacity_path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(100)
        }
    };

    let status = match status.trim() {
        "Charging" | "Full" => BatteryState::Charging,
        "Discharging" if capacity < 10 => BatteryState::Low,
        _ => BatteryState::NotCharging,
    };
    (capacity, status)
}

impl Button {
    fn with_config(cfg: ButtonConfig) -> Button {
        let id = cfg.id.clone();
        let action = ButtonAction::from_config(&cfg);
        let mut button = if let Some(text) = cfg.text {
            Button::new_text(text, action)
        } else if let Some(icon) = cfg.icon {
            Button::new_icon(
                &icon,
                cfg.theme,
                action,
                cfg.icon_width.unwrap_or(DEFAULT_ICON_SIZE),
                cfg.icon_height.unwrap_or(DEFAULT_ICON_SIZE),
            )
        } else if let Some(time) = cfg.time {
            Button::new_time(action, &time, cfg.locale.as_deref())
        } else if let Some(battery_mode) = cfg.battery {
            if let Some(battery) = find_battery_device() {
                Button::new_battery(action, battery, battery_mode, cfg.theme)
            } else {
                Button::new_text("Battery N/A".to_string(), action)
            }
        } else {
            Button::new_spacer()
        };
        button.id = id;
        button
    }
    fn new_spacer() -> Button {
        Button {
            id: None,
            action: ButtonAction::None,
            pressed: false,
            highlighted: false,
            changed: false,
            image: ButtonImage::Spacer,
            icon_width: 0.0,
            icon_height: 0.0,
        }
    }
    fn new_text(text: String, action: ButtonAction) -> Button {
        Button {
            id: None,
            action,
            pressed: false,
            highlighted: false,
            changed: false,
            image: ButtonImage::Text(text),
            icon_width: 0.0,
            icon_height: 0.0,
        }
    }
    fn new_icon(
        path: impl AsRef<str>,
        theme: Option<impl AsRef<str>>,
        action: ButtonAction,
        icon_width: i32,
        icon_height: i32,
    ) -> Button {
        let image =
            try_load_image(path, theme, icon_width, icon_height).expect("failed to load icon");
        Button {
            id: None,
            action,
            image,
            icon_width: icon_width as f64,
            icon_height: icon_height as f64,
            pressed: false,
            highlighted: false,
            changed: false,
        }
    }
    fn load_battery_image(icon: &str, theme: Option<impl AsRef<str>>) -> Handle {
        if let ButtonImage::Svg(svg) =
            try_load_image(icon, theme, DEFAULT_ICON_SIZE, DEFAULT_ICON_SIZE).unwrap()
        {
            return svg;
        }
        panic!("failed to load icon");
    }
    fn new_battery(
        action: ButtonAction,
        battery: String,
        battery_mode: String,
        theme: Option<impl AsRef<str>>,
    ) -> Button {
        let bolt = Self::load_battery_image("bolt", theme.as_ref());
        let mut plain = Vec::new();
        let mut charging = Vec::new();
        for icon in [
            "battery_0_bar",
            "battery_1_bar",
            "battery_2_bar",
            "battery_3_bar",
            "battery_4_bar",
            "battery_5_bar",
            "battery_6_bar",
            "battery_full",
        ] {
            plain.push(Self::load_battery_image(icon, theme.as_ref()));
        }
        for icon in [
            "battery_charging_20",
            "battery_charging_30",
            "battery_charging_50",
            "battery_charging_60",
            "battery_charging_80",
            "battery_charging_90",
            "battery_charging_full",
        ] {
            charging.push(Self::load_battery_image(icon, theme.as_ref()));
        }
        let battery_mode = match battery_mode.as_str() {
            "icon" => BatteryIconMode::Icon,
            "percentage" => BatteryIconMode::Percentage,
            "both" => BatteryIconMode::Both,
            _ => panic!("invalid battery mode, accepted modes: icon, percentage, both"),
        };
        Button {
            id: None,
            action,
            pressed: false,
            highlighted: false,
            changed: false,
            image: ButtonImage::Battery(
                battery,
                battery_mode,
                BatteryImages {
                    plain,
                    bolt,
                    charging,
                },
            ),
            icon_width: 0.0,
            icon_height: 0.0,
        }
    }

    fn new_time(action: ButtonAction, format: &str, locale_str: Option<&str>) -> Button {
        let format_str = if format == "24hr" {
            "%H:%M    %a %-e %b"
        } else if format == "12hr" {
            "%-l:%M %p    %a %-e %b"
        } else {
            format
        };

        let format_items = match StrftimeItems::new(format_str).parse_to_owned() {
            Ok(s) => s,
            Err(e) => panic!("Invalid time format, consult the configuration file for examples of correct ones: {e:?}"),
        };

        let locale = locale_str
            .and_then(|l| Locale::try_from(l).ok())
            .unwrap_or(Locale::POSIX);
        Button {
            id: None,
            action,
            pressed: false,
            highlighted: false,
            changed: false,
            image: ButtonImage::Time(format_items, locale),
            icon_width: 0.0,
            icon_height: 0.0,
        }
    }
    fn needs_faster_refresh(&self) -> bool {
        match &self.image {
            ButtonImage::Time(items, _) => items.iter().any(|item| {
                use chrono::format::{Item, Numeric};
                matches!(
                    item,
                    Item::Numeric(Numeric::Second, _)
                        | Item::Numeric(Numeric::Nanosecond, _)
                        | Item::Numeric(Numeric::Timestamp, _)
                )
            }),
            _ => false,
        }
    }
    fn render(
        &self,
        c: &Context,
        height: i32,
        button_left_edge: f64,
        button_width: u64,
        y_shift: f64,
    ) {
        match &self.image {
            ButtonImage::Text(text) => {
                let extents = c.text_extents(text).unwrap();
                c.move_to(
                    button_left_edge + (button_width as f64 / 2.0 - extents.width() / 2.0).round(),
                    y_shift + (height as f64 / 2.0 + extents.height() / 2.0).round(),
                );
                c.show_text(text).unwrap();
            }
            ButtonImage::Svg(svg) => {
                let x =
                    button_left_edge + (button_width as f64 / 2.0 - self.icon_width / 2.0).round();
                let y = y_shift + ((height as f64 - self.icon_height) / 2.0).round();

                svg.render_document(c, &Rectangle::new(x, y, self.icon_width, self.icon_height))
                    .unwrap();
            }
            ButtonImage::Bitmap(surf) => {
                let x =
                    button_left_edge + (button_width as f64 / 2.0 - self.icon_width / 2.0).round();
                let y = y_shift + ((height as f64 - self.icon_height) / 2.0).round();
                c.set_source_surface(surf, x, y).unwrap();
                c.rectangle(x, y, self.icon_width, self.icon_height);
                c.fill().unwrap();
            }
            ButtonImage::Time(format, locale) => {
                let current_time = Local::now();
                let formatted_time = current_time
                    .format_localized_with_items(format.iter(), *locale)
                    .to_string();
                let time_extents = c.text_extents(&formatted_time).unwrap();
                c.move_to(
                    button_left_edge
                        + (button_width as f64 / 2.0 - time_extents.width() / 2.0).round(),
                    y_shift + (height as f64 / 2.0 + time_extents.height() / 2.0).round(),
                );
                c.show_text(&formatted_time).unwrap();
            }
            ButtonImage::Battery(battery, battery_mode, icons) => {
                let (capacity, state) = get_battery_state(battery);
                let icon = if battery_mode.should_draw_icon() {
                    Some(match state {
                        BatteryState::Charging => match capacity {
                            0..=20 => &icons.charging[0],
                            21..=30 => &icons.charging[1],
                            31..=50 => &icons.charging[2],
                            51..=60 => &icons.charging[3],
                            61..=80 => &icons.charging[4],
                            81..=99 => &icons.charging[5],
                            _ => &icons.charging[6],
                        },
                        _ => match capacity {
                            0 => &icons.plain[0],
                            1..=20 => &icons.plain[1],
                            21..=30 => &icons.plain[2],
                            31..=50 => &icons.plain[3],
                            51..=60 => &icons.plain[4],
                            61..=80 => &icons.plain[5],
                            81..=99 => &icons.plain[6],
                            _ => &icons.plain[7],
                        },
                    })
                } else if state == BatteryState::Charging {
                    Some(&icons.bolt)
                } else {
                    None
                };
                let percent_str = format!("{:.0}%", capacity);
                let extents = c.text_extents(&percent_str).unwrap();
                let mut width = extents.width();
                let mut text_offset = 0;
                if let Some(svg) = icon {
                    if !battery_mode.should_draw_text() {
                        width = DEFAULT_ICON_SIZE as f64;
                    } else {
                        width += DEFAULT_ICON_SIZE as f64;
                    }
                    text_offset = DEFAULT_ICON_SIZE;
                    let x = button_left_edge + (button_width as f64 / 2.0 - width / 2.0).round();
                    let y = y_shift + ((height as f64 - DEFAULT_ICON_SIZE as f64) / 2.0).round();

                    svg.render_document(
                        c,
                        &Rectangle::new(x, y, DEFAULT_ICON_SIZE as f64, DEFAULT_ICON_SIZE as f64),
                    )
                    .unwrap();
                }
                if battery_mode.should_draw_text() {
                    c.move_to(
                        button_left_edge
                            + (button_width as f64 / 2.0 - width / 2.0 + text_offset as f64)
                                .round(),
                        y_shift + (height as f64 / 2.0 + extents.height() / 2.0).round(),
                    );
                    c.show_text(&percent_str).unwrap();
                }
            }
            ButtonImage::Spacer => (),
        }
    }
    fn is_visually_active(&self) -> bool {
        self.pressed || self.highlighted
    }

    // Used by the upcoming control socket path to update persistent visual
    // state without emitting key events.
    #[allow(dead_code)]
    fn set_highlighted(&mut self, highlighted: bool) {
        if self.highlighted != highlighted {
            self.highlighted = highlighted;
            self.changed = true;
        }
    }

    fn set_active<F>(&mut self, uinput: &mut UInputHandle<F>, active: bool)
    where
        F: AsRawFd,
    {
        if self.pressed != active {
            self.pressed = active;
            self.changed = true;

            toggle_keys(uinput, self.action.keys(), active as i32);
        }
    }
    fn set_background_color(&self, c: &Context, color: f64) {
        if let ButtonImage::Battery(battery, _, _) = &self.image {
            let (_, state) = get_battery_state(battery);
            match state {
                BatteryState::NotCharging => c.set_source_rgb(color, color, color),
                BatteryState::Charging => c.set_source_rgb(0.0, color, 0.0),
                BatteryState::Low => c.set_source_rgb(color, 0.0, 0.0),
            }
        } else {
            c.set_source_rgb(color, color, color);
        }
    }
}

#[cfg(test)]
mod button_tests {
    use super::*;

    fn text_button_config(text: &str) -> ButtonConfig {
        ButtonConfig {
            text: Some(text.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn new_button_is_not_visually_active() {
        let button = Button::new_text("workspace".to_string(), ButtonAction::None);

        assert!(!button.is_visually_active());
    }

    #[test]
    fn pressed_button_is_visually_active() {
        let mut button = Button::new_text("workspace".to_string(), ButtonAction::None);

        button.pressed = true;

        assert!(button.is_visually_active());
    }

    #[test]
    fn highlighted_button_is_visually_active_without_pressing_keys() {
        let mut button = Button::new_text("workspace".to_string(), ButtonAction::None);

        button.set_highlighted(true);

        assert!(button.is_visually_active());
        assert!(!button.pressed);
    }

    #[test]
    fn setting_highlight_marks_button_changed_without_pressing_it() {
        let mut button = Button::new_text("workspace".to_string(), ButtonAction::None);

        button.set_highlighted(true);

        assert!(button.changed);
        assert!(!button.pressed);
    }

    #[test]
    fn button_config_can_carry_a_stable_id() {
        let button = Button::with_config(ButtonConfig {
            id: Some("workspace-1".to_string()),
            ..text_button_config("1")
        });

        assert_eq!(button.id.as_deref(), Some("workspace-1"));
    }

    #[test]
    fn button_config_classifies_key_action() {
        let cfg: ButtonConfig = toml::from_str(
            r#"Text = "K"
Action = ["LeftCtrl", "F1"]"#,
        )
        .unwrap();

        assert_eq!(
            ButtonAction::from_config(&cfg),
            ButtonAction::Keys(vec![Key::LeftCtrl, Key::F1])
        );
    }

    #[test]
    fn button_config_classifies_open_overlay_without_keys() {
        let cfg: ButtonConfig = toml::from_str(
            r#"Text = "V"
Action = "VolumeUp"
OpenOverlay = "volume""#,
        )
        .unwrap();

        assert_eq!(
            ButtonAction::from_config(&cfg),
            ButtonAction::OpenOverlay("volume".to_string())
        );
    }

    #[test]
    fn button_config_classifies_close_overlay_without_keys() {
        let cfg: ButtonConfig = toml::from_str(
            r#"Text = "×"
CloseOverlay = true"#,
        )
        .unwrap();

        assert_eq!(ButtonAction::from_config(&cfg), ButtonAction::CloseOverlay);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum LayerKind {
    // The whole bar is one button row (stock behavior; the F-key layer).
    #[default]
    Classic,
    // Pinned workspace strip on the left, free middle, right-anchored
    // controls region that overlays swap in place of.
    Regions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ButtonSetKey {
    Base,
    Overlay(String),
    // Generation-tagged so a strip rebuild invalidates in-flight touches
    // instead of retargeting them onto different workspace buttons.
    Strip(u64),
}

#[derive(Default)]
struct ButtonSet {
    displays_time: bool,
    displays_battery: bool,
    buttons: Vec<(usize, Button)>,
    virtual_button_count: usize,
    faster_refresh: bool,
}

impl ButtonSet {
    fn with_config(cfg: Vec<ButtonConfig>) -> ButtonSet {
        if cfg.is_empty() {
            panic!("Invalid configuration, layer has 0 buttons");
        }

        let mut virtual_button_count = 0;
        let displays_time = cfg.iter().any(|cfg| cfg.time.is_some());
        let displays_battery = cfg.iter().any(|cfg| cfg.battery.is_some());
        let buttons = cfg
            .into_iter()
            .scan(&mut virtual_button_count, |state, cfg| {
                let i = **state;
                let mut stretch = cfg.stretch.unwrap_or(1);
                if stretch < 1 {
                    println!("Stretch value must be at least 1, setting to 1.");
                    stretch = 1;
                }
                **state += stretch;
                Some((i, Button::with_config(cfg)))
            })
            .collect::<Vec<_>>();
        let faster_refresh = buttons.iter().any(|(_, b)| b.needs_faster_refresh());
        ButtonSet {
            displays_time,
            displays_battery,
            buttons,
            virtual_button_count,
            faster_refresh,
        }
    }

    fn button_starts(&self) -> Vec<usize> {
        self.buttons.iter().map(|(start, _)| *start).collect()
    }
}

// Defensive bound on overlay nesting so a config with an OpenOverlay cycle
// cannot grow the stack without limit.
const MAX_OVERLAY_DEPTH: usize = 8;

#[derive(Default)]
pub struct FunctionLayer {
    kind: LayerKind,
    base: ButtonSet,
    overlays: HashMap<String, ButtonSet>,
    overlay_stack: Vec<String>,
    // Last time the open overlay stack was touched or changed; drives the
    // auto-close timeout.
    overlay_anchor: Option<Instant>,
    // Regions only: the pinned workspace strip. Static fallback content until
    // live compositor state arrives over the control socket.
    strip: ButtonSet,
    strip_generation: u64,
}

// Milliseconds until the overlay auto-close is due (0 = due now); None when
// the timeout is disabled or no anchor is set.
fn overlay_timeout_remaining_ms(
    anchor: Option<Instant>,
    now: Instant,
    timeout: Duration,
) -> Option<i32> {
    if timeout.is_zero() {
        return None;
    }
    let elapsed = now.saturating_duration_since(anchor?);
    let remaining = timeout.saturating_sub(elapsed);
    Some(remaining.as_millis().min(i32::MAX as u128) as i32)
}

fn fallback_strip_set(workspaces: &WorkspacesCfg) -> ButtonSet {
    let buttons = (1..=workspaces.fallback_buttons)
        .map(|idx| ButtonConfig {
            text: Some(idx.to_string()),
            action: workspaces.action_for(idx),
            ..Default::default()
        })
        .collect();
    ButtonSet::with_config(buttons)
}

impl FunctionLayer {
    fn with_config(
        cfg: Vec<ButtonConfig>,
        control_groups: HashMap<String, Vec<ButtonConfig>>,
        workspaces: Option<&WorkspacesCfg>,
    ) -> FunctionLayer {
        let base = ButtonSet::with_config(cfg);
        let overlays = control_groups
            .into_iter()
            .map(|(name, cfg)| (name, ButtonSet::with_config(cfg)))
            .collect();
        let (kind, strip) = match workspaces {
            Some(ws) => (LayerKind::Regions, fallback_strip_set(ws)),
            None => (LayerKind::Classic, ButtonSet::default()),
        };
        FunctionLayer {
            kind,
            base,
            overlays,
            overlay_stack: Vec::new(),
            overlay_anchor: None,
            strip,
            strip_generation: 0,
        }
    }

    // The key of the button set currently occupying the controls region
    // (Classic: the whole bar).
    fn controls_key(&self) -> ButtonSetKey {
        self.overlay_stack
            .last()
            .map(|name| ButtonSetKey::Overlay(name.clone()))
            .unwrap_or(ButtonSetKey::Base)
    }

    fn button_set(&self, key: &ButtonSetKey) -> Option<&ButtonSet> {
        match key {
            ButtonSetKey::Base => Some(&self.base),
            ButtonSetKey::Overlay(name) => self.overlays.get(name),
            ButtonSetKey::Strip(generation) => (self.kind == LayerKind::Regions
                && *generation == self.strip_generation)
                .then_some(&self.strip),
        }
    }

    fn button_set_mut(&mut self, key: &ButtonSetKey) -> Option<&mut ButtonSet> {
        match key {
            ButtonSetKey::Base => Some(&mut self.base),
            ButtonSetKey::Overlay(name) => self.overlays.get_mut(name),
            ButtonSetKey::Strip(generation) => (self.kind == LayerKind::Regions
                && *generation == self.strip_generation)
                .then_some(&mut self.strip),
        }
    }

    fn visible(&self) -> &ButtonSet {
        self.button_set(&self.controls_key()).unwrap()
    }

    fn visible_mut(&mut self) -> &mut ButtonSet {
        let key = self.controls_key();
        self.button_set_mut(&key).unwrap()
    }

    fn is_set_visible(&self, key: &ButtonSetKey) -> bool {
        match key {
            ButtonSetKey::Strip(generation) => {
                self.kind == LayerKind::Regions && *generation == self.strip_generation
            }
            _ => self.controls_key() == *key,
        }
    }

    fn displays_time(&self) -> bool {
        self.visible().displays_time
    }

    fn displays_battery(&self) -> bool {
        self.visible().displays_battery
    }

    fn faster_refresh(&self) -> bool {
        self.visible().faster_refresh
    }

    fn any_button_changed(&self) -> bool {
        let controls_changed = self.visible().buttons.iter().any(|b| b.1.changed);
        let strip_changed =
            self.kind == LayerKind::Regions && self.strip.buttons.iter().any(|b| b.1.changed);
        controls_changed || strip_changed
    }

    fn mark_battery_buttons_changed(&mut self) {
        for button in &mut self.visible_mut().buttons {
            if let ButtonImage::Battery(_, _, _) = button.1.image {
                button.1.changed = true;
            }
        }
    }

    fn button_mut_in_set(&mut self, key: &ButtonSetKey, index: usize) -> Option<&mut Button> {
        self.button_set_mut(key)
            .and_then(|set| set.buttons.get_mut(index))
            .map(|(_, button)| button)
    }

    fn button_action_in_set(&self, key: &ButtonSetKey, index: usize) -> Option<ButtonAction> {
        self.button_set(key)
            .and_then(|set| set.buttons.get(index))
            .map(|(_, button)| button.action.clone())
    }

    fn mark_visible_set_changed_and_unpressed(&mut self) {
        for (_, button) in &mut self.visible_mut().buttons {
            button.changed = true;
            button.pressed = false;
        }
    }

    fn open_overlay(&mut self, name: &str) -> bool {
        if !self.overlays.contains_key(name)
            || self.overlay_stack.last().is_some_and(|top| top == name)
            || self.overlay_stack.len() >= MAX_OVERLAY_DEPTH
        {
            return false;
        }
        self.overlay_stack.push(name.to_string());
        self.overlay_anchor = Some(Instant::now());
        self.mark_visible_set_changed_and_unpressed();
        true
    }

    fn close_top_overlay(&mut self) -> bool {
        if self.overlay_stack.pop().is_some() {
            self.overlay_anchor = self.has_open_overlay().then(Instant::now);
            self.mark_visible_set_changed_and_unpressed();
            true
        } else {
            false
        }
    }

    fn close_all_overlays(&mut self) -> bool {
        if self.overlay_stack.is_empty() {
            return false;
        }
        self.overlay_stack.clear();
        self.overlay_anchor = None;
        self.mark_visible_set_changed_and_unpressed();
        true
    }

    fn has_open_overlay(&self) -> bool {
        !self.overlay_stack.is_empty()
    }

    fn mark_overlay_touched(&mut self, now: Instant) {
        if self.has_open_overlay() {
            self.overlay_anchor = Some(now);
        }
    }

    fn overlay_timeout_remaining(&self, now: Instant, timeout: Duration) -> Option<i32> {
        if !self.has_open_overlay() {
            return None;
        }
        overlay_timeout_remaining_ms(self.overlay_anchor, now, timeout)
    }

    fn activate_button_action(&mut self, action: ButtonAction) -> bool {
        match action {
            ButtonAction::OpenOverlay(name) => self.open_overlay(&name),
            ButtonAction::CloseOverlay => self.close_top_overlay(),
            ButtonAction::Keys(_) | ButtonAction::None => false,
        }
    }

    // Geometry of the controls region for a given button set; the region is
    // right-anchored and never intrudes into the strip.
    fn controls_geometry(&self, set: &ButtonSet, bar_width: i32) -> RegionGeometry {
        let strip_geo = strip_region(self.strip.buttons.len());
        let min_origin = strip_geo.origin + strip_geo.width as f64 + BUTTON_SPACING_PX as f64;
        controls_region(set.virtual_button_count, bar_width, min_origin)
    }

    fn draw(
        &mut self,
        config: &Config,
        width: i32,
        height: i32,
        surface: &Surface,
        pixel_shift: (f64, f64),
        complete_redraw: bool,
    ) -> Vec<ClipRect> {
        let c = Context::new(surface).unwrap();
        let mut modified_regions = if complete_redraw {
            vec![ClipRect::new(0, 0, height as u16, width as u16)]
        } else {
            Vec::new()
        };
        c.translate(height as f64, 0.0);
        c.rotate((90.0f64).to_radians());
        let pixel_shift_width = if config.enable_pixel_shift {
            PIXEL_SHIFT_WIDTH_PX
        } else {
            0
        };
        let (pixel_shift_x, pixel_shift_y) = pixel_shift;
        let x_offset = pixel_shift_x + (pixel_shift_width / 2) as f64;
        let effective_width = width - pixel_shift_width as i32;

        if complete_redraw {
            c.set_source_rgb(0.0, 0.0, 0.0);
            c.paint().unwrap();
        }
        c.set_font_face(&config.font_face);
        c.set_font_size(32.0);

        match self.kind {
            LayerKind::Classic => {
                let visible = self.visible_mut();
                let button_starts = visible.button_starts();
                let spans = button_spans(LayoutSpec {
                    button_starts: &button_starts,
                    virtual_button_count: visible.virtual_button_count,
                    total_width: effective_width,
                    spacing_px: BUTTON_SPACING_PX,
                    x_offset,
                });
                draw_button_set(
                    &c,
                    visible,
                    &spans,
                    config,
                    height,
                    complete_redraw,
                    pixel_shift_y,
                    &mut modified_regions,
                );
            }
            LayerKind::Regions => {
                let strip_geo = strip_region(self.strip.buttons.len());
                let strip_starts = self.strip.button_starts();
                let strip_spans = button_spans(LayoutSpec {
                    button_starts: &strip_starts,
                    virtual_button_count: self.strip.virtual_button_count,
                    total_width: strip_geo.width,
                    spacing_px: STRIP_SPACING_PX,
                    x_offset: strip_geo.origin + x_offset,
                });
                draw_button_set(
                    &c,
                    &mut self.strip,
                    &strip_spans,
                    config,
                    height,
                    complete_redraw,
                    pixel_shift_y,
                    &mut modified_regions,
                );

                let controls_geo = self.controls_geometry(self.visible(), effective_width);
                let controls = self.visible_mut();
                let controls_starts = controls.button_starts();
                let controls_spans = button_spans(LayoutSpec {
                    button_starts: &controls_starts,
                    virtual_button_count: controls.virtual_button_count,
                    total_width: controls_geo.width,
                    spacing_px: BUTTON_SPACING_PX,
                    x_offset: controls_geo.origin + x_offset,
                });
                draw_button_set(
                    &c,
                    controls,
                    &controls_spans,
                    config,
                    height,
                    complete_redraw,
                    pixel_shift_y,
                    &mut modified_regions,
                );
            }
        }

        modified_regions
    }

    fn hit_in_set(
        &self,
        width: u16,
        height: u16,
        x: f64,
        y: f64,
        key: &ButtonSetKey,
        i: Option<usize>,
    ) -> Option<usize> {
        if !self.is_set_visible(key) {
            return None;
        }
        let button_set = self.button_set(key)?;
        let button_starts = button_set.button_starts();
        // Regions hit-test in region-local coordinates; a tap left of the
        // region maps to a negative x_rel, which hit_index rejects on the
        // span bounds check.
        let (total_width, spacing_px, x_rel) = match (self.kind, key) {
            (LayerKind::Regions, ButtonSetKey::Strip(_)) => {
                let geo = strip_region(button_set.buttons.len());
                (geo.width, STRIP_SPACING_PX, x - geo.origin)
            }
            (LayerKind::Regions, _) => {
                let geo = self.controls_geometry(button_set, width as i32);
                (geo.width, BUTTON_SPACING_PX, x - geo.origin)
            }
            (LayerKind::Classic, _) => (width as i32, BUTTON_SPACING_PX, x),
        };
        hit_index(
            LayoutSpec {
                button_starts: &button_starts,
                virtual_button_count: button_set.virtual_button_count,
                total_width,
                spacing_px,
                x_offset: 0.0,
            },
            height,
            x_rel,
            y,
            i,
        )
    }

    #[cfg(test)]
    fn hit(&self, width: u16, height: u16, x: f64, y: f64, i: Option<usize>) -> Option<usize> {
        self.hit_in_set(width, height, x, y, &self.controls_key(), i)
    }

    fn hit_target(&self, width: u16, height: u16, x: f64, y: f64) -> HitOutcome {
        if self.kind == LayerKind::Regions {
            let strip_key = ButtonSetKey::Strip(self.strip_generation);
            if let Some(index) = self.hit_in_set(width, height, x, y, &strip_key, None) {
                return HitOutcome::Button(strip_key, index);
            }
        }
        let key = self.controls_key();
        if let Some(index) = self.hit_in_set(width, height, x, y, &key, None) {
            return HitOutcome::Button(key, index);
        }
        if self.kind == LayerKind::Regions && !self.overlay_stack.is_empty() {
            HitOutcome::OutsideControls
        } else {
            HitOutcome::Miss
        }
    }
}

// A touch-down classified against the visible regions. OutsideControls only
// occurs in Regions mode with an overlay open; the upcoming tap-outside close
// path consumes it.
enum HitOutcome {
    Button(ButtonSetKey, usize),
    OutsideControls,
    Miss,
}

#[allow(clippy::too_many_arguments)]
fn draw_button_set(
    c: &Context,
    set: &mut ButtonSet,
    spans: &[ButtonSpan],
    config: &Config,
    height: i32,
    complete_redraw: bool,
    pixel_shift_y: f64,
    modified_regions: &mut Vec<ClipRect>,
) {
    let radius = 8.0f64;
    let bot = (height as f64) * 0.15;
    let top = (height as f64) * 0.85;

    for span in spans {
        let button = &mut set.buttons[span.index].1;

        if !button.changed && !complete_redraw {
            continue;
        };

        let left_edge = span.left_edge;
        let button_width = span.width;

        let color = if button.is_visually_active() {
            BUTTON_COLOR_ACTIVE
        } else if config.show_button_outlines {
            BUTTON_COLOR_INACTIVE
        } else {
            0.0
        };
        if !complete_redraw {
            c.set_source_rgb(0.0, 0.0, 0.0);
            c.rectangle(
                left_edge,
                bot - radius,
                button_width,
                top - bot + radius * 2.0,
            );
            c.fill().unwrap();
        }
        if !matches!(button.image, ButtonImage::Spacer) {
            button.set_background_color(c, color);
            c.new_sub_path();
            let left = left_edge + radius;
            let right = (left_edge + button_width.ceil()) - radius;
            c.arc(
                right,
                bot,
                radius,
                (-90.0f64).to_radians(),
                (0.0f64).to_radians(),
            );
            c.arc(
                right,
                top,
                radius,
                (0.0f64).to_radians(),
                (90.0f64).to_radians(),
            );
            c.arc(
                left,
                top,
                radius,
                (90.0f64).to_radians(),
                (180.0f64).to_radians(),
            );
            c.arc(
                left,
                bot,
                radius,
                (180.0f64).to_radians(),
                (270.0f64).to_radians(),
            );
            c.close_path();
            c.fill().unwrap();
        }
        c.set_source_rgb(1.0, 1.0, 1.0);
        button.render(
            c,
            height,
            left_edge,
            button_width.ceil() as u64,
            pixel_shift_y,
        );

        button.changed = false;

        if !complete_redraw {
            modified_regions.push(ClipRect::new(
                height as u16 - top as u16 - radius as u16,
                left_edge as u16,
                height as u16 - bot as u16 + radius as u16,
                left_edge as u16 + button_width as u16,
            ));
        }
    }
}

#[cfg(test)]
mod function_layer_tests {
    use super::*;

    fn text_button(text: &str, action: ButtonAction) -> ButtonConfig {
        let (open_overlay, close_overlay, keys) = match action {
            ButtonAction::Keys(keys) => (None, None, keys),
            ButtonAction::OpenOverlay(name) => (Some(name), None, vec![]),
            ButtonAction::CloseOverlay => (None, Some(true), vec![]),
            ButtonAction::None => (None, None, vec![]),
        };
        ButtonConfig {
            text: Some(text.to_string()),
            action: keys,
            open_overlay,
            close_overlay,
            ..Default::default()
        }
    }

    #[test]
    fn opening_overlay_changes_visible_hit_behavior() {
        let mut groups = HashMap::new();
        groups.insert(
            "volume".to_string(),
            vec![text_button("up", ButtonAction::None)],
        );
        let mut layer = FunctionLayer::with_config(
            vec![
                text_button("open", ButtonAction::OpenOverlay("volume".to_string())),
                text_button("base", ButtonAction::None),
            ],
            groups,
            None,
        );

        assert_eq!(layer.hit(100, 20, 75.0, 10.0, None), Some(1));
        assert!(layer.open_overlay("volume"));
        assert_eq!(layer.hit(100, 20, 75.0, 10.0, None), Some(0));
    }

    #[test]
    fn closing_overlay_returns_to_base() {
        let mut groups = HashMap::new();
        groups.insert(
            "volume".to_string(),
            vec![text_button("close", ButtonAction::CloseOverlay)],
        );
        let mut layer = FunctionLayer::with_config(
            vec![
                text_button("open", ButtonAction::OpenOverlay("volume".to_string())),
                text_button("base", ButtonAction::None),
            ],
            groups,
            None,
        );

        assert!(layer.open_overlay("volume"));
        assert_eq!(layer.hit(100, 20, 75.0, 10.0, None), Some(0));
        assert!(layer.close_top_overlay());
        assert_eq!(layer.hit(100, 20, 75.0, 10.0, None), Some(1));
    }

    #[test]
    fn key_action_does_not_change_overlay_state() {
        let mut layer = FunctionLayer::with_config(
            vec![text_button("key", ButtonAction::Keys(vec![Key::F1]))],
            HashMap::new(),
            None,
        );

        assert!(!layer.activate_button_action(ButtonAction::Keys(vec![Key::F1])));
        assert!(layer.overlay_stack.is_empty());
    }

    fn nested_layer() -> FunctionLayer {
        let mut groups = HashMap::new();
        groups.insert(
            "sound".to_string(),
            vec![
                text_button("vol", ButtonAction::OpenOverlay("volume".to_string())),
                text_button("play", ButtonAction::None),
            ],
        );
        groups.insert(
            "volume".to_string(),
            vec![text_button("slider", ButtonAction::None)],
        );
        FunctionLayer::with_config(
            vec![text_button(
                "open",
                ButtonAction::OpenOverlay("sound".to_string()),
            )],
            groups,
            None,
        )
    }

    #[test]
    fn nested_open_shows_innermost_overlay() {
        let mut layer = nested_layer();

        assert!(layer.open_overlay("sound"));
        assert!(layer.open_overlay("volume"));

        assert_eq!(
            layer.controls_key(),
            ButtonSetKey::Overlay("volume".to_string())
        );
    }

    #[test]
    fn close_all_returns_to_base_from_depth_two() {
        let mut layer = nested_layer();
        assert!(layer.open_overlay("sound"));
        assert!(layer.open_overlay("volume"));

        assert!(layer.close_all_overlays());

        assert_eq!(layer.controls_key(), ButtonSetKey::Base);
        assert!(!layer.close_all_overlays());
    }

    #[test]
    fn close_overlay_action_pops_one_level() {
        let mut layer = nested_layer();
        assert!(layer.open_overlay("sound"));
        assert!(layer.open_overlay("volume"));

        assert!(layer.activate_button_action(ButtonAction::CloseOverlay));

        assert_eq!(
            layer.controls_key(),
            ButtonSetKey::Overlay("sound".to_string())
        );
    }

    #[test]
    fn reopening_top_overlay_is_a_noop() {
        let mut layer = nested_layer();
        assert!(layer.open_overlay("sound"));

        assert!(!layer.open_overlay("sound"));

        assert_eq!(layer.overlay_stack.len(), 1);
    }

    #[test]
    fn overlay_stack_depth_is_bounded() {
        let mut layer = nested_layer();
        for _ in 0..2 * MAX_OVERLAY_DEPTH {
            layer.open_overlay("sound");
            layer.open_overlay("volume");
        }

        assert!(layer.overlay_stack.len() <= MAX_OVERLAY_DEPTH);
    }

    #[test]
    fn opening_missing_overlay_is_rejected() {
        let mut layer = nested_layer();

        assert!(!layer.open_overlay("does-not-exist"));

        assert_eq!(layer.controls_key(), ButtonSetKey::Base);
    }

    const BAR_WIDTH: u16 = 2008;
    const BAR_HEIGHT: u16 = 60;

    fn ws_cfg(fallback_buttons: usize) -> WorkspacesCfg {
        WorkspacesCfg {
            max_buttons: 9,
            fallback_buttons,
            actions: vec![],
        }
    }

    fn regions_layer(fallback_buttons: usize) -> FunctionLayer {
        let mut groups = HashMap::new();
        groups.insert(
            "canary".to_string(),
            vec![text_button("x", ButtonAction::None)],
        );
        FunctionLayer::with_config(
            vec![
                text_button("open", ButtonAction::OpenOverlay("canary".to_string())),
                text_button("play", ButtonAction::Keys(vec![Key::F2])),
            ],
            groups,
            Some(&ws_cfg(fallback_buttons)),
        )
    }

    #[test]
    fn fallback_strip_has_single_unhighlighted_workspace_one() {
        let layer = regions_layer(1);

        assert_eq!(layer.strip.buttons.len(), 1);
        let button = &layer.strip.buttons[0].1;
        assert!(matches!(&button.image, ButtonImage::Text(t) if t == "1"));
        assert_eq!(
            button.action,
            ButtonAction::Keys(vec![Key::LeftAlt, Key::Num1])
        );
        assert!(!button.highlighted);
    }

    #[test]
    fn strip_hits_use_region_local_coordinates() {
        let layer = regions_layer(4);

        // Strip: origin 12, four 60px buttons with 10px gaps.
        assert!(matches!(
            layer.hit_target(BAR_WIDTH, BAR_HEIGHT, 42.0, 30.0),
            HitOutcome::Button(ButtonSetKey::Strip(0), 0)
        ));
        assert!(matches!(
            layer.hit_target(BAR_WIDTH, BAR_HEIGHT, 112.0, 30.0),
            HitOutcome::Button(ButtonSetKey::Strip(0), 1)
        ));
        // Controls: two launchers right-anchored at 2008 - 12 - 236 = 1760.
        assert!(matches!(
            layer.hit_target(BAR_WIDTH, BAR_HEIGHT, 1765.0, 30.0),
            HitOutcome::Button(ButtonSetKey::Base, 0)
        ));
        assert!(matches!(
            layer.hit_target(BAR_WIDTH, BAR_HEIGHT, 1990.0, 30.0),
            HitOutcome::Button(ButtonSetKey::Base, 1)
        ));
        // The free middle is nothing while no overlay is open.
        assert!(matches!(
            layer.hit_target(BAR_WIDTH, BAR_HEIGHT, 1000.0, 30.0),
            HitOutcome::Miss
        ));
    }

    #[test]
    fn outside_tap_with_overlay_open_is_outside_controls() {
        let mut layer = regions_layer(4);
        assert!(layer.open_overlay("canary"));

        assert!(matches!(
            layer.hit_target(BAR_WIDTH, BAR_HEIGHT, 1000.0, 30.0),
            HitOutcome::OutsideControls
        ));
        // Strip buttons stay live targets while the overlay is open.
        assert!(matches!(
            layer.hit_target(BAR_WIDTH, BAR_HEIGHT, 42.0, 30.0),
            HitOutcome::Button(ButtonSetKey::Strip(0), 0)
        ));
    }

    #[test]
    fn stale_strip_generation_is_not_visible() {
        let mut layer = regions_layer(4);
        let stale = ButtonSetKey::Strip(layer.strip_generation);
        layer.strip_generation += 1;

        assert!(!layer.is_set_visible(&stale));
        assert!(layer.button_set(&stale).is_none());
        assert_eq!(
            layer.hit_in_set(BAR_WIDTH, BAR_HEIGHT, 42.0, 30.0, &stale, Some(0)),
            None
        );
        assert!(layer.is_set_visible(&ButtonSetKey::Strip(layer.strip_generation)));
    }

    fn null_uinput() -> UInputHandle<File> {
        UInputHandle::new(OpenOptions::new().write(true).open("/dev/null").unwrap())
    }

    #[test]
    fn timeout_due_after_configured_idle() {
        let t0 = Instant::now();
        let timeout = Duration::from_millis(8000);

        assert_eq!(
            overlay_timeout_remaining_ms(Some(t0), t0, timeout),
            Some(8000)
        );
        assert_eq!(
            overlay_timeout_remaining_ms(Some(t0), t0 + Duration::from_millis(3000), timeout),
            Some(5000)
        );
        assert_eq!(
            overlay_timeout_remaining_ms(Some(t0), t0 + Duration::from_millis(9000), timeout),
            Some(0)
        );
    }

    #[test]
    fn zero_timeout_never_fires() {
        let t0 = Instant::now();

        assert_eq!(
            overlay_timeout_remaining_ms(Some(t0), t0 + Duration::from_secs(3600), Duration::ZERO),
            None
        );
    }

    #[test]
    fn touch_refreshes_timeout_anchor() {
        let mut layer = regions_layer(4);
        let timeout = Duration::from_millis(8000);
        assert!(layer.open_overlay("canary"));
        let t0 = Instant::now();
        layer.mark_overlay_touched(t0);

        layer.mark_overlay_touched(t0 + Duration::from_millis(7000));

        assert_eq!(
            layer.overlay_timeout_remaining(t0 + Duration::from_millis(8000), timeout),
            Some(7000)
        );
    }

    #[test]
    fn timeout_is_inert_without_an_open_overlay() {
        let mut layer = regions_layer(4);
        let timeout = Duration::from_millis(8000);
        assert!(layer.open_overlay("canary"));
        assert!(layer.close_all_overlays());

        assert_eq!(
            layer.overlay_timeout_remaining(Instant::now(), timeout),
            None
        );
        // A stray touch after close must not resurrect the anchor.
        layer.mark_overlay_touched(Instant::now());
        assert_eq!(
            layer.overlay_timeout_remaining(Instant::now(), timeout),
            None
        );
    }

    #[test]
    fn drain_releases_all_tracked_touches() {
        let mut uinput = null_uinput();
        let mut layers = [regions_layer(4), regions_layer(4)];
        let mut touches = HashMap::new();

        let strip_key = ButtonSetKey::Strip(0);
        layers[0]
            .button_mut_in_set(&strip_key, 0)
            .unwrap()
            .set_active(&mut uinput, true);
        touches.insert(1, (0usize, strip_key.clone(), 0usize));
        layers[0]
            .button_mut_in_set(&ButtonSetKey::Base, 1)
            .unwrap()
            .set_active(&mut uinput, true);
        touches.insert(2, (0usize, ButtonSetKey::Base, 1usize));

        drain_touches(&mut layers, &mut touches, &mut uinput);

        assert!(touches.is_empty());
        assert!(
            !layers[0].button_set(&strip_key).unwrap().buttons[0]
                .1
                .pressed
        );
        assert!(
            !layers[0].button_set(&ButtonSetKey::Base).unwrap().buttons[1]
                .1
                .pressed
        );
    }

    #[test]
    fn drain_survives_touches_on_stale_button_sets() {
        let mut uinput = null_uinput();
        let mut layers = [regions_layer(4), regions_layer(4)];
        let mut touches = HashMap::new();
        touches.insert(1, (0usize, ButtonSetKey::Strip(99), 0usize));
        touches.insert(2, (0usize, ButtonSetKey::Overlay("gone".into()), 0usize));

        drain_touches(&mut layers, &mut touches, &mut uinput);

        assert!(touches.is_empty());
    }

    #[test]
    fn classic_layer_has_no_strip_targets() {
        let layer = FunctionLayer::with_config(
            vec![text_button("a", ButtonAction::None)],
            HashMap::new(),
            None,
        );

        assert_eq!(layer.kind, LayerKind::Classic);
        assert!(layer.button_set(&ButtonSetKey::Strip(0)).is_none());
        // Full-bar hit routing still works.
        assert!(matches!(
            layer.hit_target(BAR_WIDTH, BAR_HEIGHT, 1000.0, 30.0),
            HitOutcome::Button(ButtonSetKey::Base, 0)
        ));
    }

    #[test]
    fn hidden_button_set_does_not_retarget_active_touch() {
        let mut groups = HashMap::new();
        groups.insert(
            "volume".to_string(),
            vec![text_button("up", ButtonAction::None)],
        );
        let mut layer = FunctionLayer::with_config(
            vec![
                text_button("open", ButtonAction::OpenOverlay("volume".to_string())),
                text_button("base", ButtonAction::Keys(vec![Key::F1])),
            ],
            groups,
            None,
        );
        let base = layer.controls_key();

        assert_eq!(
            layer.hit_in_set(100, 20, 75.0, 10.0, &base, Some(1)),
            Some(1)
        );
        assert!(layer.open_overlay("volume"));

        assert_eq!(layer.hit(100, 20, 75.0, 10.0, None), Some(0));
        assert_eq!(layer.hit_in_set(100, 20, 75.0, 10.0, &base, Some(1)), None);
        assert!(layer.button_mut_in_set(&base, 1).is_some());
    }
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let mode = flags & O_ACCMODE;

        OpenOptions::new()
            .custom_flags(flags)
            .read(mode == O_RDONLY || mode == O_RDWR)
            .write(mode == O_WRONLY || mode == O_RDWR)
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap())
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        _ = File::from(fd);
    }
}

fn emit<F>(uinput: &mut UInputHandle<F>, ty: EventKind, code: u16, value: i32)
where
    F: AsRawFd,
{
    uinput
        .write(&[input_event {
            value,
            type_: ty as u16,
            code,
            time: timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
        }])
        .unwrap();
}

fn toggle_keys<F>(uinput: &mut UInputHandle<F>, codes: &[Key], value: i32)
where
    F: AsRawFd,
{
    if codes.is_empty() {
        return;
    }
    for kc in codes {
        emit(uinput, EventKind::Key, *kc as u16, value);
    }
    emit(
        uinput,
        EventKind::Synchronize,
        SynchronizeKind::Report as u16,
        0,
    );
}

// The single invalidation path for the touch map: every route that hides or
// replaces buttons (overlay transitions, timeout close, layer flips, config
// reloads) must release in-flight touches through here so no virtual key is
// ever stranded down.
fn drain_touches<K, F>(
    layers: &mut [FunctionLayer; 2],
    touches: &mut HashMap<K, (usize, ButtonSetKey, usize)>,
    uinput: &mut UInputHandle<F>,
) where
    K: Eq + std::hash::Hash,
    F: AsRawFd,
{
    for (_, (layer, set, btn)) in touches.drain() {
        if let Some(button) = layers[layer].button_mut_in_set(&set, btn) {
            button.set_active(uinput, false);
        }
    }
}

fn main() {
    let mut drm = DrmBackend::open_card().unwrap();
    let (height, width) = drm.mode().size();
    let _ = panic::catch_unwind(AssertUnwindSafe(|| real_main(&mut drm)));
    let crash_bitmap = include_bytes!("crash_bitmap.raw");
    let mut map = drm.map().unwrap();
    let data = map.as_mut();
    let mut wptr = 0;
    for byte in crash_bitmap {
        for i in 0..8 {
            let bit = ((byte >> i) & 0x1) == 0;
            let color = if bit { 0xFF } else { 0x0 };
            data[wptr] = color;
            data[wptr + 1] = color;
            data[wptr + 2] = color;
            data[wptr + 3] = color;
            wptr += 4;
        }
    }
    drop(map);
    drm.dirty(&[ClipRect::new(0, 0, height, width)]).unwrap();
    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGTERM);
    sigset.wait().unwrap();
}

fn real_main(drm: &mut DrmBackend) {
    let (height, width) = drm.mode().size();
    let (db_width, db_height) = drm.fb_info().unwrap().size();
    let mut uinput = UInputHandle::new(OpenOptions::new().write(true).open("/dev/uinput").unwrap());
    let mut backlight = BacklightManager::new();
    let mut cfg_mgr = ConfigManager::new();
    let (mut cfg, mut layers) = cfg_mgr.load_config(width);
    let mut pixel_shift = PixelShiftManager::new();
    let mut last = Instant::now();

    // drop privileges to input and video group
    let groups = ["input", "video"];

    PrivDrop::default()
        .user("nobody")
        .group_list(&groups)
        .apply()
        .unwrap_or_else(|e| panic!("Failed to drop privileges: {}", e));

    let mut surface =
        ImageSurface::create(Format::ARgb32, db_width as i32, db_height as i32).unwrap();
    let mut active_layer = 0;
    let mut needs_complete_redraw = true;

    let mut input_tb = Libinput::new_with_udev(Interface);
    let mut input_main = Libinput::new_with_udev(Interface);
    input_tb.udev_assign_seat("seat-touchbar").unwrap();
    input_main.udev_assign_seat("seat0").unwrap();
    let udev_monitor = MonitorBuilder::new()
        .unwrap()
        .match_subsystem("power_supply")
        .unwrap()
        .listen()
        .unwrap();
    let epoll = Epoll::new(EpollCreateFlags::empty()).unwrap();
    epoll
        .add(input_main.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 0))
        .unwrap();
    epoll
        .add(input_tb.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 1))
        .unwrap();
    epoll
        .add(cfg_mgr.fd(), EpollEvent::new(EpollFlags::EPOLLIN, 2))
        .unwrap();
    epoll
        .add(&udev_monitor, EpollEvent::new(EpollFlags::EPOLLIN, 3))
        .unwrap();
    uinput.set_evbit(EventKind::Key).unwrap();
    for k in Key::iter() {
        uinput.set_keybit(k).unwrap();
    }
    let mut dev_name_c = [0 as c_char; 80];
    let dev_name = "Dynamic Function Row Virtual Input Device".as_bytes();
    for i in 0..dev_name.len() {
        dev_name_c[i] = dev_name[i] as c_char;
    }
    uinput
        .dev_setup(&uinput_setup {
            id: input_id {
                bustype: 0x19,
                vendor: 0x1209,
                product: 0x316E,
                version: 1,
            },
            ff_effects_max: 0,
            name: dev_name_c,
        })
        .unwrap();
    uinput.dev_create().unwrap();

    let mut digitizer: Option<InputDevice> = None;
    let mut touches = HashMap::new();
    let mut last_redraw_ts = if layers[active_layer].faster_refresh() {
        Local::now().second()
    } else {
        Local::now().minute()
    };
    loop {
        if cfg_mgr.reload_pending() {
            // Release in-flight touches against the outgoing layers before
            // they are dropped, or their key-down events would be stranded.
            drain_touches(&mut layers, &mut touches, &mut uinput);
            let parts = cfg_mgr.load_config(width);
            cfg = parts.0;
            layers = parts.1;
            active_layer = 0;
            needs_complete_redraw = true;
        }

        let now = Local::now();
        let ms_left = ((60 - now.second()) * 1000) as i32;
        let mut next_timeout_ms = min(ms_left, TIMEOUT_MS);

        if cfg.enable_pixel_shift {
            let (pixel_shift_needs_redraw, pixel_shift_next_timeout_ms) = pixel_shift.update();
            if pixel_shift_needs_redraw {
                needs_complete_redraw = true;
            }
            next_timeout_ms = min(next_timeout_ms, pixel_shift_next_timeout_ms);
        }

        if let Some(remaining) =
            layers[active_layer].overlay_timeout_remaining(Instant::now(), cfg.overlay_timeout)
        {
            if remaining > 0 {
                next_timeout_ms = min(next_timeout_ms, remaining);
            } else if touches.is_empty() {
                // Never auto-close under an active touch; the anchor refreshes
                // on release, restarting the idle window.
                if layers[active_layer].close_all_overlays() {
                    needs_complete_redraw = true;
                }
            }
        }

        let current_ts = if layers[active_layer].faster_refresh() {
            Local::now().second()
        } else {
            Local::now().minute()
        };
        if layers[active_layer].displays_time() && (current_ts != last_redraw_ts) {
            needs_complete_redraw = true;
            last_redraw_ts = current_ts;
        }
        if layers[active_layer].displays_battery() {
            layers[active_layer].mark_battery_buttons_changed();
        }

        if needs_complete_redraw || layers[active_layer].any_button_changed() {
            let shift = if cfg.enable_pixel_shift {
                pixel_shift.get()
            } else {
                (0.0, 0.0)
            };
            let clips = layers[active_layer].draw(
                &cfg,
                width as i32,
                height as i32,
                &surface,
                shift,
                needs_complete_redraw,
            );
            let data = surface.data().unwrap();
            drm.map().unwrap().as_mut()[..data.len()].copy_from_slice(&data);
            drm.dirty(&clips).unwrap();
            needs_complete_redraw = false;
        }

        match epoll.wait(
            &mut [EpollEvent::new(EpollFlags::EPOLLIN, 0)],
            next_timeout_ms as u16,
        ) {
            Err(Errno::EINTR) | Ok(_) => 0,
            e => e.unwrap(),
        };

        _ = udev_monitor.iter().last();

        input_tb.dispatch().unwrap();
        input_main.dispatch().unwrap();
        for event in &mut input_tb.clone().chain(input_main.clone()) {
            backlight.process_event(&event);
            match event {
                Event::Device(DeviceEvent::Added(evt)) => {
                    let dev = evt.device();
                    if dev.name().contains(" Touch Bar") {
                        digitizer = Some(dev);
                    }
                }
                Event::Keyboard(KeyboardEvent::Key(key)) => {
                    if key.key() == Key::Fn as u32 {
                        if cfg.double_press_switch_layers > 0
                            && key.key_state() == KeyState::Pressed
                        {
                            if last.elapsed()
                                < Duration::from_millis(cfg.double_press_switch_layers.into())
                            {
                                // Swapping invalidates the layer indices held
                                // by in-flight touches; release them first.
                                drain_touches(&mut layers, &mut touches, &mut uinput);
                                layers.swap(0, 1);
                            }
                            last = Instant::now();
                        }
                        let new_layer = match key.key_state() {
                            KeyState::Pressed => 1,
                            KeyState::Released => 0,
                        };
                        if active_layer != new_layer {
                            drain_touches(&mut layers, &mut touches, &mut uinput);
                            layers[active_layer].close_all_overlays();
                            active_layer = new_layer;
                            needs_complete_redraw = true;
                        }
                    }
                }
                Event::Touch(te) => {
                    if Some(te.device()) != digitizer || backlight.current_bl() == 0 {
                        continue;
                    }
                    // Any touch on the bar counts as overlay activity.
                    layers[active_layer].mark_overlay_touched(Instant::now());
                    match te {
                        TouchEvent::Down(dn) => {
                            let x = dn.x_transformed(width as u32);
                            let y = dn.y_transformed(height as u32);
                            match layers[active_layer].hit_target(width, height, x, y) {
                                HitOutcome::Button(button_set, btn) => {
                                    // A workspace tap while an overlay is open
                                    // closes the whole stack AND performs the
                                    // switch in the same tap.
                                    if matches!(button_set, ButtonSetKey::Strip(_))
                                        && layers[active_layer].has_open_overlay()
                                    {
                                        drain_touches(&mut layers, &mut touches, &mut uinput);
                                        layers[active_layer].close_all_overlays();
                                        needs_complete_redraw = true;
                                    }
                                    touches.insert(dn.seat_slot(), (active_layer, button_set, btn));
                                    let (layer, button_set, btn) =
                                        touches.get(&dn.seat_slot()).unwrap().clone();
                                    if let Some(button) =
                                        layers[layer].button_mut_in_set(&button_set, btn)
                                    {
                                        button.set_active(&mut uinput, true);
                                    }
                                }
                                // iPhone-folder rule: touching anywhere outside
                                // the overlay's buttons dismisses the stack on
                                // Down. The touch is not recorded, so its
                                // release cannot activate anything.
                                HitOutcome::OutsideControls => {
                                    drain_touches(&mut layers, &mut touches, &mut uinput);
                                    if layers[active_layer].close_all_overlays() {
                                        needs_complete_redraw = true;
                                    }
                                }
                                HitOutcome::Miss => {}
                            }
                        }
                        TouchEvent::Motion(mtn) => {
                            if !touches.contains_key(&mtn.seat_slot()) {
                                continue;
                            }

                            let x = mtn.x_transformed(width as u32);
                            let y = mtn.y_transformed(height as u32);
                            let (layer, button_set, btn) =
                                touches.get(&mtn.seat_slot()).unwrap().clone();
                            let hit = layers[layer]
                                .hit_in_set(width, height, x, y, &button_set, Some(btn))
                                .is_some();
                            if let Some(button) = layers[layer].button_mut_in_set(&button_set, btn)
                            {
                                button.set_active(&mut uinput, hit);
                            }
                        }
                        TouchEvent::Up(up) => {
                            if !touches.contains_key(&up.seat_slot()) {
                                continue;
                            }
                            let (layer, button_set, btn) = touches.remove(&up.seat_slot()).unwrap();
                            let action = layers[layer].button_action_in_set(&button_set, btn);
                            let can_activate = layers[layer].is_set_visible(&button_set);
                            let mut hit = false;
                            if let Some(button) = layers[layer].button_mut_in_set(&button_set, btn)
                            {
                                hit = button.pressed;
                                button.set_active(&mut uinput, false);
                            }
                            if hit && can_activate {
                                if let Some(action) = action {
                                    if layers[layer].activate_button_action(action) {
                                        drain_touches(&mut layers, &mut touches, &mut uinput);
                                        needs_complete_redraw = true;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        backlight.update_backlight(&cfg);
    }
}
