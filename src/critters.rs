use cairo::Context;
use drm::control::ClipRect;
use librsvg_rebind::{prelude::HandleExt, Handle, Rectangle};
use rand::Rng;
use std::time::{Duration, Instant};

// Pet Claudes: one Claude Code sprite per running session, walking the bar's
// free middle region like a tiny 2D platformer — strictly left/right along
// the floor, with a little hop in the stride. Render-only: no hit spans, no
// keys, no intents; they exist only while the helper reports sessions. The
// animation runs at ~15fps via the event loop's timeout clamp (the
// pixel-shift pattern) and only while critters are visible.

pub const CRITTER_FRAME: Duration = Duration::from_millis(66);
// Bound a tick's dt so a suspend/resume gap doesn't teleport anyone.
const MAX_TICK_DT: Duration = Duration::from_millis(250);
const WALK_SPEED_MIN: f64 = 25.0; // px/s
const WALK_SPEED_MAX: f64 = 60.0;
const TURN_CHANCE_PER_SEC: f64 = 0.25;
const HOP_HEIGHT: f64 = 4.0;

// The embedded Claude Code glyph (lobehub icon, recolored terracotta) fills
// the x-range of its 24-unit viewBox but only y 5..20. Scale the render rect
// so the VISIBLE sprite stands ~46px tall (~77% of the 60px bar) with its
// feet on the floor at y=58.
const VIEWBOX: f64 = 24.0;
const GLYPH_TOP: f64 = 5.0;
const GLYPH_BOTTOM: f64 = 20.0;
const SPRITE_HEIGHT: f64 = 46.0;
const FLOOR_Y: f64 = 58.0;
const RECT_SIZE: f64 = SPRITE_HEIGHT * VIEWBOX / (GLYPH_BOTTOM - GLYPH_TOP);
const HALF_WIDTH: f64 = RECT_SIZE / 2.0 + 1.0;

pub const CLAUDE_CRITTER_SVG: &[u8] = include_bytes!("../share/tiny-dfr/claude-critter.svg");

struct Critter {
    id: String,
    x: f64,
    vx: f64,
    phase: f64, // stride cycle accumulator
}

pub struct CritterField {
    sprite: Option<Handle>,
    critters: Vec<Critter>,
    // Horizontal spans drawn last frame, pending erasure.
    prev_spans: Vec<(f64, f64)>,
    last_tick: Option<Instant>,
}

impl CritterField {
    pub fn new() -> CritterField {
        let sprite = match Handle::from_data(CLAUDE_CRITTER_SVG) {
            Ok(handle) => Some(handle),
            Err(e) => {
                eprintln!("claude critter sprite failed to load: {e}");
                None
            }
        };
        CritterField {
            sprite,
            critters: Vec::new(),
            prev_spans: Vec::new(),
            last_tick: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.critters.is_empty() || self.sprite.is_none()
    }

    pub fn needs_erase(&self) -> bool {
        !self.prev_spans.is_empty()
    }

    // Sync the population with the helper's session list: survivors keep
    // their position mid-stride, newcomers wander in, departed ones vanish.
    // Returns true when the population changed.
    pub fn reconcile(&mut self, ids: &[String], region: (f64, f64)) -> bool {
        let before = self.critters.len();
        self.critters.retain(|critter| ids.contains(&critter.id));
        let mut rng = rand::thread_rng();
        for id in ids {
            if self.critters.iter().any(|c| &c.id == id) {
                continue;
            }
            let (start, end) = region;
            let span = (end - start - 2.0 * HALF_WIDTH).max(1.0);
            let speed = rng.gen_range(WALK_SPEED_MIN..WALK_SPEED_MAX);
            self.critters.push(Critter {
                id: id.clone(),
                x: start + HALF_WIDTH + rng.gen_range(0.0..1.0) * span,
                vx: if rng.gen_bool(0.5) { speed } else { -speed },
                phase: rng.gen_range(0.0..std::f64::consts::TAU),
            });
        }
        self.critters.len() != before || self.critters.len() != ids.len()
    }

    // Advance the walk if a frame has elapsed; true = positions moved.
    // Motion is strictly horizontal; the floor is the floor.
    pub fn tick(&mut self, now: Instant, region: (f64, f64)) -> bool {
        if self.critters.is_empty() {
            self.last_tick = None;
            return false;
        }
        let Some(last) = self.last_tick else {
            self.last_tick = Some(now);
            return true; // first frame after (re)activation
        };
        let elapsed = now.saturating_duration_since(last);
        if elapsed < CRITTER_FRAME {
            return false;
        }
        self.last_tick = Some(now);
        let dt = elapsed.min(MAX_TICK_DT).as_secs_f64();
        let (start, end) = region;
        let (lo, hi) = (
            start + HALF_WIDTH,
            (end - HALF_WIDTH).max(start + HALF_WIDTH),
        );
        let mut rng = rand::thread_rng();
        for critter in &mut self.critters {
            critter.x += critter.vx * dt;
            if critter.x <= lo {
                critter.x = lo;
                critter.vx = critter.vx.abs();
            } else if critter.x >= hi {
                critter.x = hi;
                critter.vx = -critter.vx.abs();
            } else if rng.gen_bool((TURN_CHANCE_PER_SEC * dt).clamp(0.0, 1.0)) {
                critter.vx = -critter.vx;
            }
            // Stride cadence scales with speed so faster claudes hop faster.
            critter.phase += dt * (4.0 + critter.vx.abs() * 0.12);
        }
        true
    }

    // How long the event loop may sleep before the next frame is due.
    pub fn wait_ms(&self, now: Instant) -> Option<i32> {
        if self.critters.is_empty() {
            return None;
        }
        let last = self.last_tick?;
        let remaining = CRITTER_FRAME.saturating_sub(now.saturating_duration_since(last));
        Some(remaining.as_millis().max(1) as i32)
    }

    // Erase last frame's spans and draw the current population (clamped into
    // `region`, or nothing when None). The caller's context must carry the
    // bar's rotation transform. Returns damage rects; on a complete redraw
    // the background is already fresh so no erasure damage is needed.
    pub fn render(
        &mut self,
        c: &Context,
        height: i32,
        bar_width: i32,
        region: Option<(f64, f64)>,
        complete_redraw: bool,
    ) -> Vec<ClipRect> {
        let mut clips = Vec::new();
        if !complete_redraw {
            for span in self.prev_spans.drain(..) {
                c.set_source_rgb(0.0, 0.0, 0.0);
                c.rectangle(span.0, TOP_Y, span.1 - span.0, BOTTOM_Y - TOP_Y);
                c.fill().unwrap();
                clips.push(clip_for_span(span, height, bar_width));
            }
        } else {
            self.prev_spans.clear();
        }

        let (Some((start, end)), Some(sprite)) = (region, self.sprite.as_ref()) else {
            return clips;
        };
        let (lo, hi) = (
            start + HALF_WIDTH,
            (end - HALF_WIDTH).max(start + HALF_WIDTH),
        );
        for critter in &self.critters {
            let x = critter.x.clamp(lo, hi);
            // Platformer stride: the sprite bounces off the floor.
            let hop = critter.phase.sin().abs() * HOP_HEIGHT;
            let rect_y = FLOOR_Y - hop - GLYPH_BOTTOM / VIEWBOX * RECT_SIZE;
            let _ = sprite.render_document(
                c,
                &Rectangle::new(x - RECT_SIZE / 2.0, rect_y, RECT_SIZE, RECT_SIZE),
            );
            let span = (x - HALF_WIDTH, x + HALF_WIDTH);
            clips.push(clip_for_span(span, height, bar_width));
            self.prev_spans.push(span);
        }
        clips
    }
}

impl Default for CritterField {
    fn default() -> CritterField {
        CritterField::new()
    }
}

// Vertical extent of the sprite (incl. hop headroom) in drawing space.
const TOP_Y: f64 = 6.0;
const BOTTOM_Y: f64 = 59.0;

fn clip_for_span(span: (f64, f64), height: i32, bar_width: i32) -> ClipRect {
    let x1 = span.0.floor().clamp(0.0, bar_width as f64) as u16;
    let x2 = span.1.ceil().clamp(0.0, bar_width as f64) as u16;
    // Same fb mapping as draw_button_set: fb x = height - drawing y.
    ClipRect::new(
        (height as f64 - BOTTOM_Y).max(0.0) as u16,
        x1,
        (height as f64 - TOP_Y) as u16,
        x2,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairo::{Format, ImageSurface};

    const REGION: (f64, f64) = (300.0, 1700.0);

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn embedded_sprite_loads() {
        let field = CritterField::new();
        assert!(field.sprite.is_some());
    }

    #[test]
    fn reconcile_spawns_within_region_and_despawns() {
        let mut field = CritterField::new();

        assert!(field.reconcile(&ids(&["a", "b"]), REGION));
        assert_eq!(field.critters.len(), 2);
        for critter in &field.critters {
            assert!(critter.x >= REGION.0 + HALF_WIDTH);
            assert!(critter.x <= REGION.1 - HALF_WIDTH + 1.0);
        }

        let survivor_x = field.critters[0].x;
        assert!(field.reconcile(&ids(&["a"]), REGION));
        assert_eq!(field.critters.len(), 1);
        // Survivors keep their position mid-stride.
        assert_eq!(field.critters[0].x, survivor_x);

        assert!(!field.reconcile(&ids(&["a"]), REGION));
    }

    #[test]
    fn tick_paces_frames_and_stays_in_bounds() {
        let mut field = CritterField::new();
        field.reconcile(&ids(&["a"]), REGION);
        let t0 = Instant::now();

        assert!(field.tick(t0, REGION)); // first frame
        assert!(!field.tick(t0 + Duration::from_millis(10), REGION)); // too soon
        assert!(field.tick(t0 + Duration::from_millis(80), REGION));

        // A long walk never escapes the region, even across a fake suspend.
        let mut now = t0 + Duration::from_millis(80);
        for _ in 0..200 {
            now += Duration::from_millis(70);
            field.tick(now, REGION);
        }
        field.tick(now + Duration::from_secs(3600), REGION); // resume gap
        let x = field.critters[0].x;
        assert!(x >= REGION.0 + HALF_WIDTH && x <= REGION.1 - HALF_WIDTH);
    }

    #[test]
    fn render_erases_previous_spans_and_reports_damage() {
        let surface = ImageSurface::create(Format::ARgb32, 60, 2008).unwrap();
        let c = Context::new(&surface).unwrap();
        c.translate(60.0, 0.0);
        c.rotate((90.0f64).to_radians());
        let mut field = CritterField::new();
        field.reconcile(&ids(&["a"]), REGION);

        let first = field.render(&c, 60, 2008, Some(REGION), true);
        assert_eq!(first.len(), 1); // draw only; complete redraw skips erase
        assert!(field.needs_erase());

        let second = field.render(&c, 60, 2008, Some(REGION), false);
        assert_eq!(second.len(), 2); // erase old + draw new

        // Deactivation (no region) erases and reports that damage.
        let third = field.render(&c, 60, 2008, None, false);
        assert_eq!(third.len(), 1);
        assert!(!field.needs_erase());
    }

    #[test]
    fn wait_ms_is_bounded_by_the_frame_interval() {
        let mut field = CritterField::new();
        assert_eq!(field.wait_ms(Instant::now()), None);

        field.reconcile(&ids(&["a"]), REGION);
        let t0 = Instant::now();
        field.tick(t0, REGION);
        let wait = field.wait_ms(t0 + Duration::from_millis(6)).unwrap();
        assert!(wait >= 1 && wait <= CRITTER_FRAME.as_millis() as i32);
    }
}
