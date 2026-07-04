use crate::helper_proto::NowPlaying;
use cairo::{Antialias, Context, Format, ImageSurface};
use drm::control::ClipRect;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

const ART_SIZE_PX: i32 = 36;
const MAX_ART_BYTES: u64 = 2 * 1024 * 1024;
const MAX_WIDGET_WIDTH_PX: f64 = 440.0;
const MIN_WIDGET_WIDTH_PX: f64 = 180.0;
const PADDING_PX: f64 = 10.0;
const TEXT_GAP_PX: f64 = 12.0;
const TITLE_FONT_SIZE: f64 = 19.0;
const ARTIST_FONT_SIZE: f64 = 13.0;
const CONTROL_GAP_PX: f64 = 16.0;
const BUTTON_BACKGROUND: f64 = 0.200;

#[derive(Default)]
pub(crate) struct NowPlayingRenderer {
    cached_art_path: Option<String>,
    cached_art: Option<ImageSurface>,
}

impl NowPlayingRenderer {
    pub(crate) fn render(
        &mut self,
        c: &Context,
        height: i32,
        bar_width: i32,
        region: Option<(f64, f64)>,
        controls_origin: Option<f64>,
        media: Option<&NowPlaying>,
    ) -> Vec<ClipRect> {
        let (Some(media), Some((region_start, region_end))) = (media, region) else {
            return Vec::new();
        };
        let right_edge = controls_origin
            .map(|origin| (origin - CONTROL_GAP_PX).min(region_end))
            .unwrap_or(region_end);
        let region_width = right_edge - region_start;
        if region_width < MIN_WIDGET_WIDTH_PX {
            return Vec::new();
        }

        let widget_width = region_width.min(MAX_WIDGET_WIDTH_PX);
        let x = right_edge - widget_width;
        let bot = (height as f64) * 0.15;
        let top = (height as f64) * 0.85;
        let widget_height = top - bot;
        let y = bot;
        let art = self.art_for(media.art_path.as_deref());
        let text_left = x
            + PADDING_PX
            + if art.is_some() {
                ART_SIZE_PX as f64 + TEXT_GAP_PX
            } else {
                0.0
            };
        let text_right = x + widget_width - PADDING_PX;
        let text_width = text_right - text_left;
        if text_width < 24.0 {
            return Vec::new();
        }

        c.save().unwrap();
        draw_round_rect(c, x, y, widget_width, widget_height, 8.0);
        c.set_source_rgb(BUTTON_BACKGROUND, BUTTON_BACKGROUND, BUTTON_BACKGROUND);
        c.fill().unwrap();

        if let Some(art) = art {
            let art_x = x + PADDING_PX;
            let art_y = y + ((widget_height - ART_SIZE_PX as f64) / 2.0).round();
            c.save().unwrap();
            draw_round_rect(c, art_x, art_y, ART_SIZE_PX as f64, ART_SIZE_PX as f64, 7.0);
            c.clip();
            c.set_source_surface(art, art_x, art_y).unwrap();
            c.paint().unwrap();
            c.restore().unwrap();
        }

        c.set_source_rgb(1.0, 1.0, 1.0);
        if media.artist.is_empty() {
            c.set_font_size(TITLE_FONT_SIZE);
            let title = ellipsize(c, &media.title, text_width);
            let extents = c.text_extents(&title).unwrap();
            c.move_to(
                text_left,
                y + (widget_height / 2.0 + extents.height() / 2.0).round(),
            );
            c.show_text(&title).unwrap();
        } else {
            c.set_font_size(TITLE_FONT_SIZE);
            let title = ellipsize(c, &media.title, text_width);
            c.move_to(text_left, y + 17.0);
            c.show_text(&title).unwrap();

            c.set_font_size(ARTIST_FONT_SIZE);
            c.set_source_rgb(0.78, 0.78, 0.78);
            let artist = ellipsize(c, &media.artist, text_width);
            c.move_to(text_left, y + 32.0);
            c.show_text(&artist).unwrap();
        }
        c.restore().unwrap();

        vec![clip_for_span(height, bar_width, x, widget_width)]
    }

    fn art_for(&mut self, path: Option<&str>) -> Option<&ImageSurface> {
        let path_changed = self.cached_art_path.as_deref() != path;
        if path_changed || (path.is_some() && self.cached_art.is_none()) {
            self.cached_art_path = path.map(str::to_string);
            self.cached_art = path.and_then(load_art);
        }
        self.cached_art.as_ref()
    }
}

fn draw_round_rect(c: &Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    use std::f64::consts::PI;
    let radius = radius.min(width / 2.0).min(height / 2.0);
    c.new_sub_path();
    c.arc(x + width - radius, y + radius, radius, 1.5 * PI, 2.0 * PI);
    c.arc(
        x + width - radius,
        y + height - radius,
        radius,
        0.0,
        0.5 * PI,
    );
    c.arc(x + radius, y + height - radius, radius, 0.5 * PI, PI);
    c.arc(x + radius, y + radius, radius, PI, 1.5 * PI);
    c.close_path();
}

fn ellipsize(c: &Context, text: &str, max_width: f64) -> String {
    if c.text_extents(text)
        .is_ok_and(|ext| ext.width() <= max_width)
    {
        return text.to_string();
    }

    let mut truncated: String = text.chars().take(80).collect();
    while !truncated.is_empty() {
        let candidate = format!("{truncated}...");
        if c.text_extents(&candidate)
            .is_ok_and(|ext| ext.width() <= max_width)
        {
            return candidate;
        }
        truncated.pop();
    }
    String::new()
}

fn clip_for_span(height: i32, bar_width: i32, left: f64, width: f64) -> ClipRect {
    let x1 = left.floor().clamp(0.0, bar_width as f64) as u16;
    let x2 = (left + width).ceil().clamp(0.0, bar_width as f64) as u16;
    ClipRect::new(0, x1, height as u16, x2)
}

fn load_art(path: &str) -> Option<ImageSurface> {
    let path = safe_art_path(path)?;
    if fs::metadata(&path).ok()?.len() > MAX_ART_BYTES {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let surf = ImageSurface::create_from_png(&mut file).ok()?;
    if surf.width() <= 0 || surf.height() <= 0 {
        return None;
    }

    let resized = ImageSurface::create(Format::ARgb32, ART_SIZE_PX, ART_SIZE_PX).ok()?;
    let c = Context::new(&resized).ok()?;
    c.set_antialias(Antialias::Best);
    let scale =
        (ART_SIZE_PX as f64 / surf.width() as f64).max(ART_SIZE_PX as f64 / surf.height() as f64);
    let dest_w = surf.width() as f64 * scale;
    let dest_h = surf.height() as f64 * scale;
    c.translate(
        (ART_SIZE_PX as f64 - dest_w) / 2.0,
        (ART_SIZE_PX as f64 - dest_h) / 2.0,
    );
    c.scale(scale, scale);
    c.set_source_surface(surf, 0.0, 0.0).ok()?;
    c.paint().ok()?;
    Some(resized)
}

fn safe_art_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if !path.is_absolute() || path.extension().and_then(|ext| ext.to_str()) != Some("png") {
        return None;
    }
    let canonical = fs::canonicalize(path).ok()?;
    if canonical.starts_with("/tmp/tiny-dfr-ben")
        || canonical.starts_with("/run/tiny-dfr-ben/media")
    {
        Some(canonical)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_art_test_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "tiny-dfr-ben/now-playing-test-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn clip_for_span_is_clamped_to_bar() {
        assert_eq!(
            clip_for_span(60, 2008, -10.0, 50.0),
            ClipRect::new(0, 0, 60, 40)
        );
        assert_eq!(
            clip_for_span(60, 2008, 1990.0, 100.0),
            ClipRect::new(0, 1990, 60, 2008)
        );
    }

    #[test]
    fn safe_art_path_rejects_relative_and_wrong_extension() {
        assert!(safe_art_path("cover.png").is_none());
        assert!(safe_art_path("/tmp/tiny-dfr-ben/cover.jpg").is_none());
    }

    #[test]
    fn safe_art_path_accepts_allowed_root_and_rejects_symlink_escape() {
        let dir = unique_art_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let allowed = dir.join("cover.png");
        fs::write(&allowed, b"not decoded in this test").unwrap();
        assert_eq!(safe_art_path(allowed.to_str().unwrap()), Some(allowed));

        let outside =
            std::env::temp_dir().join(format!("tiny-dfr-outside-cover-{}.png", std::process::id()));
        fs::write(&outside, b"outside").unwrap();
        let link = dir.join("escape.png");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(safe_art_path(link.to_str().unwrap()).is_none());

        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(dir);
    }
}
