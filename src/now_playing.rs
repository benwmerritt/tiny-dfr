use crate::helper_proto::{NowPlaying, MEDIA_ART_DIR};
use cairo::{Antialias, Context, Format, ImageSurface};
use drm::control::ClipRect;
use std::{
    fs::{self, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const ART_SIZE_PX: i32 = 42;
const MAX_ART_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ART_DIMENSION_PX: u32 = 512;
const MAX_WIDGET_WIDTH_PX: f64 = 440.0;
const MIN_WIDGET_WIDTH_PX: f64 = 180.0;
const LEFT_PADDING_PX: f64 = 10.0;
const RIGHT_PADDING_PX: f64 = 14.0;
const TEXT_GAP_PX: f64 = 12.0;
const TITLE_FONT_SIZE: f64 = 19.0;
const ARTIST_FONT_SIZE: f64 = 13.0;
const CONTROL_GAP_PX: f64 = 16.0;
const BUTTON_BACKGROUND: f64 = 0.200;
const BUTTON_RADIUS_PX: f64 = 8.0;
const ART_RETRY_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct NowPlayingRenderer {
    cached_art_media: Option<NowPlaying>,
    cached_art: Option<ImageSurface>,
    art_retry_at: Option<Instant>,
    last_bounds: Option<WidgetBounds>,
    bounds_generation: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct NowPlayingLayout {
    pub(crate) region: Option<(f64, f64)>,
    pub(crate) controls_origin: Option<f64>,
    pub(crate) y_shift: f64,
}

impl NowPlayingRenderer {
    pub(crate) fn render(
        &mut self,
        c: &Context,
        height: i32,
        bar_width: i32,
        layout: NowPlayingLayout,
        media: Option<&NowPlaying>,
    ) -> Vec<ClipRect> {
        let (Some(media), Some((region_start, region_end))) = (media, layout.region) else {
            self.set_bounds(None);
            return Vec::new();
        };
        let right_edge = layout
            .controls_origin
            .map(|origin| (origin - CONTROL_GAP_PX).min(region_end))
            .unwrap_or(region_end);
        let region_width = right_edge - region_start;
        if region_width < MIN_WIDGET_WIDTH_PX {
            self.set_bounds(None);
            return Vec::new();
        }

        let has_art = self.art_for(media).is_some();
        let measured_text_width = measure_text_width(c, media);
        let Some(widget_width) = widget_width_for(region_width, has_art, measured_text_width)
        else {
            self.set_bounds(None);
            return Vec::new();
        };
        let x = right_edge - widget_width;
        let (y, widget_height) = button_frame(height, layout.y_shift);
        let text_left = x
            + LEFT_PADDING_PX
            + if has_art {
                ART_SIZE_PX as f64 + TEXT_GAP_PX
            } else {
                0.0
            };
        let text_right = x + widget_width - RIGHT_PADDING_PX;
        let text_width = text_right - text_left;
        if text_width < 24.0 {
            self.set_bounds(None);
            return Vec::new();
        }

        c.save().unwrap();
        draw_round_rect(c, x, y, widget_width, widget_height, BUTTON_RADIUS_PX);
        c.set_source_rgb(BUTTON_BACKGROUND, BUTTON_BACKGROUND, BUTTON_BACKGROUND);
        c.fill().unwrap();

        if let Some(art) = self.cached_art.as_ref() {
            let art_x = x + LEFT_PADDING_PX;
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
            c.move_to(text_left, y + (widget_height / 2.0 - 4.0).round());
            c.show_text(&title).unwrap();

            c.set_font_size(ARTIST_FONT_SIZE);
            c.set_source_rgb(0.78, 0.78, 0.78);
            let artist = ellipsize(c, &media.artist, text_width);
            c.move_to(text_left, y + (widget_height / 2.0 + 13.0).round());
            c.show_text(&artist).unwrap();
        }
        c.restore().unwrap();

        self.set_bounds(Some(WidgetBounds::new(x, y, widget_width, widget_height)));
        vec![clip_for_span(height, bar_width, x, widget_width)]
    }

    pub(crate) fn hit_test(&self, x: f64, y: f64) -> bool {
        self.last_bounds.is_some_and(|bounds| bounds.contains(x, y))
    }

    pub(crate) fn touch_token(&self, x: f64, y: f64) -> Option<u64> {
        self.hit_test(x, y).then_some(self.bounds_generation)
    }

    pub(crate) fn token_is_current(&self, token: u64) -> bool {
        self.last_bounds.is_some() && token == self.bounds_generation
    }

    fn art_for(&mut self, media: &NowPlaying) -> Option<&ImageSurface> {
        let now = Instant::now();
        if self.art_cache_needs_reload(media, now) {
            self.cached_art_media = Some(media.clone());
            self.cached_art = media.art_path.as_deref().and_then(load_art);
            self.art_retry_at = (media.art_path.is_some() && self.cached_art.is_none())
                .then_some(now + ART_RETRY_INTERVAL);
        }
        self.cached_art.as_ref()
    }

    fn art_cache_needs_reload(&self, media: &NowPlaying, now: Instant) -> bool {
        self.cached_art_media.as_ref() != Some(media)
            || self.art_retry_at.is_some_and(|retry_at| now >= retry_at)
    }

    pub(crate) fn art_retry_wait(
        &self,
        media: Option<&NowPlaying>,
        now: Instant,
    ) -> Option<Duration> {
        let media = media?;
        if self.last_bounds.is_none()
            || self.cached_art_media.as_ref() != Some(media)
            || self.cached_art.is_some()
            || media.art_path.is_none()
        {
            return None;
        }
        self.art_retry_at
            .map(|retry_at| retry_at.saturating_duration_since(now))
    }

    fn set_bounds(&mut self, bounds: Option<WidgetBounds>) {
        let invalidates_touch = match (self.last_bounds, bounds) {
            (None, None) => false,
            (Some(old), Some(new)) => old.width() != new.width() || old.height() != new.height(),
            _ => true,
        };
        self.last_bounds = bounds;
        if invalidates_touch {
            self.bounds_generation = self.bounds_generation.wrapping_add(1);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct WidgetBounds {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl WidgetBounds {
    fn new(left: f64, top: f64, width: f64, height: f64) -> WidgetBounds {
        WidgetBounds {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.left && x <= self.right && y >= self.top && y <= self.bottom
    }

    fn width(self) -> f64 {
        self.right - self.left
    }

    fn height(self) -> f64 {
        self.bottom - self.top
    }
}

fn measure_text_width(c: &Context, media: &NowPlaying) -> f64 {
    c.save().unwrap();
    c.set_font_size(TITLE_FONT_SIZE);
    let title_width = c
        .text_extents(&media.title)
        .map(|ext| ext.width())
        .unwrap_or(0.0);
    let artist_width = if media.artist.is_empty() {
        0.0
    } else {
        c.set_font_size(ARTIST_FONT_SIZE);
        c.text_extents(&media.artist)
            .map(|ext| ext.width())
            .unwrap_or(0.0)
    };
    c.restore().unwrap();
    title_width.max(artist_width)
}

fn widget_width_for(region_width: f64, has_art: bool, measured_text_width: f64) -> Option<f64> {
    if region_width < MIN_WIDGET_WIDTH_PX {
        return None;
    }
    let max_widget_width = region_width.min(MAX_WIDGET_WIDTH_PX);
    let non_text_width = LEFT_PADDING_PX
        + RIGHT_PADDING_PX
        + if has_art {
            ART_SIZE_PX as f64 + TEXT_GAP_PX
        } else {
            0.0
        };
    let max_text_width = (max_widget_width - non_text_width).max(24.0);
    let text_width = measured_text_width.max(24.0).min(max_text_width);
    Some(
        (non_text_width + text_width)
            .ceil()
            .clamp(MIN_WIDGET_WIDTH_PX, max_widget_width),
    )
}

fn button_frame(height: i32, y_shift: f64) -> (f64, f64) {
    let bot = (height as f64) * 0.15;
    let top = (height as f64) * 0.85;
    (
        bot - BUTTON_RADIUS_PX + y_shift,
        top - bot + BUTTON_RADIUS_PX * 2.0,
    )
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
    // The helper owns the media directory, so validate the object actually
    // opened rather than trusting path metadata. O_NONBLOCK prevents a FIFO
    // from stalling the render/input loop; O_NOFOLLOW rejects a final-component
    // symlink even if it is swapped in after the lexical path check.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(&path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_ART_BYTES {
        return None;
    }
    // The helper can still mutate an already-open inode. Snapshot through the
    // verified descriptor with a hard read cap, then validate and decode only
    // the immutable bytes so growth/rewrite races cannot bypass either bound.
    let bytes = read_art_snapshot(file)?;
    let mut image = Cursor::new(bytes);
    // The compressed-byte cap does not stop a tiny PNG from declaring a huge
    // decoded surface. Check IHDR before Cairo allocates for the image.
    if !png_dimensions_are_safe(&mut image) {
        return None;
    }
    let surf = ImageSurface::create_from_png(&mut image).ok()?;
    if surf.width() <= 0
        || surf.height() <= 0
        || surf.width() as u32 > MAX_ART_DIMENSION_PX
        || surf.height() as u32 > MAX_ART_DIMENSION_PX
    {
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

fn read_art_snapshot<R: Read>(reader: R) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_ART_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_ART_BYTES).then_some(bytes)
}

fn png_dimensions_are_safe<R: Read + Seek>(reader: &mut R) -> bool {
    let mut header = [0u8; 24];
    if reader.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }
    let read_ok = reader.read_exact(&mut header).is_ok();
    let rewind_ok = reader.seek(SeekFrom::Start(0)).is_ok();
    if !read_ok || !rewind_ok {
        return false;
    }
    if &header[..8] != b"\x89PNG\r\n\x1a\n"
        || u32::from_be_bytes(header[8..12].try_into().unwrap()) != 13
        || &header[12..16] != b"IHDR"
    {
        return false;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(header[20..24].try_into().unwrap());
    width > 0 && height > 0 && width <= MAX_ART_DIMENSION_PX && height <= MAX_ART_DIMENSION_PX
}

fn safe_art_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if !path.is_absolute() || path.extension().and_then(|ext| ext.to_str()) != Some("png") {
        return None;
    }
    if path.parent() != Some(Path::new(MEDIA_ART_DIR)) && !test_art_path_allowed(path) {
        return None;
    }
    if fs::symlink_metadata(path).ok()?.file_type().is_symlink() {
        return None;
    }
    Some(path.to_path_buf())
}

#[cfg(test)]
fn test_art_path_allowed(path: &Path) -> bool {
    path.starts_with("/tmp/tiny-dfr-ben")
}

#[cfg(not(test))]
fn test_art_path_allowed(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::CString,
        io::Cursor,
        os::unix::ffi::OsStrExt,
        time::{SystemTime, UNIX_EPOCH},
    };

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
    fn button_frame_matches_control_body_height() {
        assert_eq!(button_frame(60, 0.0), (1.0, 58.0));
        assert_eq!(button_frame(60, 2.0), (3.0, 58.0));
    }

    #[test]
    fn widget_width_tracks_content_until_capped() {
        assert_eq!(widget_width_for(800.0, true, 90.0), Some(180.0));
        assert_eq!(widget_width_for(800.0, true, 130.0), Some(208.0));
        assert_eq!(
            widget_width_for(800.0, true, 900.0),
            Some(MAX_WIDGET_WIDTH_PX)
        );
        assert_eq!(widget_width_for(320.0, true, 900.0), Some(320.0));
        assert_eq!(widget_width_for(160.0, true, 90.0), None);
    }

    #[test]
    fn widget_bounds_hit_test_includes_edges() {
        let bounds = WidgetBounds::new(100.0, 1.0, 180.0, 58.0);

        assert!(bounds.contains(100.0, 1.0));
        assert!(bounds.contains(280.0, 59.0));
        assert!(bounds.contains(180.0, 30.0));
        assert!(!bounds.contains(99.9, 30.0));
        assert!(!bounds.contains(180.0, 59.1));
    }

    #[test]
    fn bounds_generation_invalidates_an_in_flight_touch() {
        let mut renderer = NowPlayingRenderer::default();
        renderer.set_bounds(Some(WidgetBounds::new(100.0, 1.0, 180.0, 58.0)));
        let token = renderer.touch_token(180.0, 30.0).unwrap();
        assert!(renderer.token_is_current(token));

        // Pixel shifting moves the same interactive shape and should not make
        // ordinary presses fail merely because they span an animation tick.
        renderer.set_bounds(Some(WidgetBounds::new(110.0, 3.0, 180.0, 58.0)));
        assert!(renderer.token_is_current(token));
        assert!(renderer.touch_token(180.0, 30.0).is_some());

        // A content/geometry change or visibility change is semantic and must
        // invalidate the bounds captured at Down.
        renderer.set_bounds(Some(WidgetBounds::new(110.0, 3.0, 181.0, 58.0)));
        assert!(!renderer.token_is_current(token));
        let token = renderer.touch_token(180.0, 30.0).unwrap();
        renderer.set_bounds(None);
        assert!(!renderer.token_is_current(token));
    }

    #[test]
    fn art_cache_reloads_on_media_change_and_throttles_failures() {
        let now = Instant::now();
        let path = Some("/run/tiny-dfr-ben/media/current.png".to_string());
        let first = NowPlaying {
            title: "First track".to_string(),
            artist: "Artist".to_string(),
            art_path: path.clone(),
        };
        let second = NowPlaying {
            title: "Second track".to_string(),
            artist: "Artist".to_string(),
            art_path: path,
        };
        let mut renderer = NowPlayingRenderer {
            cached_art_media: Some(first.clone()),
            cached_art: Some(ImageSurface::create(Format::ARgb32, 1, 1).unwrap()),
            last_bounds: Some(WidgetBounds::new(100.0, 1.0, 180.0, 58.0)),
            ..Default::default()
        };

        assert!(!renderer.art_cache_needs_reload(&first, now));
        assert!(renderer.art_cache_needs_reload(&second, now));
        renderer.cached_art = None;
        renderer.art_retry_at = Some(now + ART_RETRY_INTERVAL);
        assert!(!renderer.art_cache_needs_reload(&first, now));
        assert_eq!(
            renderer.art_retry_wait(Some(&first), now),
            Some(ART_RETRY_INTERVAL)
        );
        assert!(renderer.art_cache_needs_reload(&first, now + ART_RETRY_INTERVAL));
        renderer.last_bounds = None;
        assert_eq!(
            renderer.art_retry_wait(Some(&first), now + ART_RETRY_INTERVAL),
            None
        );
    }

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    #[test]
    fn png_dimensions_are_bounded_before_decode() {
        assert!(png_dimensions_are_safe(&mut Cursor::new(png_header(
            96, 96
        ))));
        assert!(!png_dimensions_are_safe(&mut Cursor::new(png_header(
            0, 96
        ))));
        assert!(!png_dimensions_are_safe(&mut Cursor::new(png_header(
            MAX_ART_DIMENSION_PX + 1,
            1,
        ))));
        assert!(!png_dimensions_are_safe(&mut Cursor::new(
            b"not png".to_vec()
        )));
    }

    #[test]
    fn art_loader_rejects_fifo_without_blocking() {
        let dir = unique_art_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("cover.png");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_c is a valid, NUL-terminated path owned by this test.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(load_art(fifo.to_str().unwrap()).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn art_snapshot_enforces_the_compressed_byte_cap() {
        assert_eq!(
            read_art_snapshot(Cursor::new(vec![0u8; MAX_ART_BYTES as usize]))
                .unwrap()
                .len(),
            MAX_ART_BYTES as usize
        );
        assert!(read_art_snapshot(Cursor::new(vec![0u8; MAX_ART_BYTES as usize + 1])).is_none());
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
