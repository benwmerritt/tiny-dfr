// Region geometry for LayerKind::Regions: a compact square workspace strip on
// the left, free space in the middle, and a right-anchored controls region.
// The bar draws in rotated space as roughly 2008x60 on this hardware, with a
// 42px button band, so 60px-wide strip buttons read as squares.
pub(crate) const STRIP_BUTTON_WIDTH_PX: i32 = 60;
pub(crate) const STRIP_SPACING_PX: i32 = 10;
pub(crate) const STRIP_LEFT_MARGIN_PX: f64 = 12.0;
pub(crate) const CONTROL_UNIT_PX: i32 = 110;
pub(crate) const CONTROL_RIGHT_MARGIN_PX: f64 = 12.0;
pub(crate) const CONTROL_SPACING_PX: i32 = 16;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RegionGeometry {
    pub(crate) origin: f64,
    pub(crate) width: i32,
}

pub(crate) fn strip_region(n_buttons: usize) -> RegionGeometry {
    let n = n_buttons.max(1) as i32;
    RegionGeometry {
        origin: STRIP_LEFT_MARGIN_PX,
        width: n * STRIP_BUTTON_WIDTH_PX + (n - 1) * STRIP_SPACING_PX,
    }
}

// Controls region placement. `anchor_x` is the desired (unclamped) origin —
// the launcher position an overlay expands from; None means right-anchored
// (the base controls row). Never intrudes into [0, min_origin) so the strip
// stays untouched even under a pathologically wide controls config; in that
// degenerate branch the anchor is ignored entirely.
pub(crate) fn controls_region(
    virtual_count: usize,
    bar_width: i32,
    min_origin: f64,
    anchor_x: Option<f64>,
) -> RegionGeometry {
    let n = virtual_count.max(1) as i32;
    let width = n * CONTROL_UNIT_PX + (n - 1) * CONTROL_SPACING_PX;
    let max_origin = bar_width as f64 - CONTROL_RIGHT_MARGIN_PX - width as f64;
    if max_origin < min_origin {
        let clamped_width = (bar_width as f64 - CONTROL_RIGHT_MARGIN_PX - min_origin) as i32;
        return RegionGeometry {
            origin: min_origin,
            width: clamped_width.max(1),
        };
    }
    let origin = anchor_x
        .filter(|a| a.is_finite())
        .map(|a| a.clamp(min_origin, max_origin))
        .unwrap_or(max_origin);
    RegionGeometry { origin, width }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LayoutSpec<'a> {
    pub(crate) button_starts: &'a [usize],
    pub(crate) virtual_button_count: usize,
    pub(crate) total_width: i32,
    pub(crate) spacing_px: i32,
    pub(crate) x_offset: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ButtonSpan {
    pub(crate) index: usize,
    pub(crate) virtual_start: usize,
    pub(crate) virtual_end: usize,
    pub(crate) left_edge: f64,
    pub(crate) width: f64,
}

fn virtual_button_width(total_width: i32, spacing_px: i32, virtual_button_count: usize) -> f64 {
    (total_width - (spacing_px * (virtual_button_count - 1) as i32)) as f64
        / virtual_button_count as f64
}

pub(crate) fn button_spans(spec: LayoutSpec<'_>) -> Vec<ButtonSpan> {
    let virtual_button_width =
        virtual_button_width(spec.total_width, spec.spacing_px, spec.virtual_button_count);

    spec.button_starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = spec
                .button_starts
                .get(index + 1)
                .copied()
                .unwrap_or(spec.virtual_button_count);
            let left_edge = (*start as f64 * (virtual_button_width + spec.spacing_px as f64))
                .floor()
                + spec.x_offset;
            let width = virtual_button_width
                + ((end - start - 1) as f64 * (virtual_button_width + spec.spacing_px as f64))
                    .floor();

            ButtonSpan {
                index,
                virtual_start: *start,
                virtual_end: end,
                left_edge,
                width,
            }
        })
        .collect()
}

pub(crate) fn hit_index(
    spec: LayoutSpec<'_>,
    total_height: u16,
    x: f64,
    y: f64,
    constrained_index: Option<usize>,
) -> Option<usize> {
    let index = constrained_index.unwrap_or_else(|| {
        let virtual_i = (x / (spec.total_width as f64 / spec.virtual_button_count as f64)) as usize;
        spec.button_starts
            .iter()
            .position(|start| *start > virtual_i)
            .unwrap_or(spec.button_starts.len())
            - 1
    });

    let span = button_spans(spec)
        .into_iter()
        .find(|span| span.index == index)?;

    if x < span.left_edge
        || x > (span.left_edge + span.width)
        || y < 0.1 * total_height as f64
        || y > 0.9 * total_height as f64
    {
        return None;
    }

    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACING: i32 = 10;

    fn spec(
        button_starts: &[usize],
        virtual_button_count: usize,
        total_width: i32,
    ) -> LayoutSpec<'_> {
        LayoutSpec {
            button_starts,
            virtual_button_count,
            total_width,
            spacing_px: SPACING,
            x_offset: 0.0,
        }
    }

    #[test]
    fn button_spans_include_stretched_widths_and_spacers() {
        let spans = button_spans(spec(&[0, 1, 3], 4, 100));

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].virtual_start, 0);
        assert_eq!(spans[0].virtual_end, 1);
        assert_eq!(spans[0].left_edge, 0.0);
        assert_eq!(spans[0].width, 17.5);

        assert_eq!(spans[1].virtual_start, 1);
        assert_eq!(spans[1].virtual_end, 3);
        assert_eq!(spans[1].left_edge, 27.0);
        assert_eq!(spans[1].width, 44.5);

        assert_eq!(spans[2].virtual_start, 3);
        assert_eq!(spans[2].virtual_end, 4);
        assert_eq!(spans[2].left_edge, 82.0);
        assert_eq!(spans[2].width, 17.5);
    }

    #[test]
    fn button_spans_apply_x_offset_without_changing_widths() {
        let starts = [0, 2];
        let spans = button_spans(LayoutSpec {
            x_offset: 12.5,
            ..spec(&starts, 3, 120)
        });

        assert_eq!(spans[0].left_edge, 12.5);
        assert_eq!(spans[0].width, 76.33333333333334);
        assert_eq!(spans[1].left_edge, 98.5);
        assert_eq!(spans[1].width, 33.333333333333336);
    }

    #[test]
    fn hit_index_accepts_first_and_last_button_bounds() {
        let starts = [0, 1, 3];

        assert_eq!(
            hit_index(spec(&starts, 4, 100), 20, 0.0, 10.0, None),
            Some(0)
        );
        assert_eq!(
            hit_index(spec(&starts, 4, 100), 20, 99.5, 10.0, None),
            Some(2)
        );
    }

    #[test]
    fn hit_index_rejects_spacing_and_vertical_misses() {
        let starts = [0, 1, 3];

        assert_eq!(hit_index(spec(&starts, 4, 100), 20, 20.0, 10.0, None), None);
        assert_eq!(hit_index(spec(&starts, 4, 100), 20, 50.0, 1.0, None), None);
        assert_eq!(hit_index(spec(&starts, 4, 100), 20, 50.0, 19.0, None), None);
    }

    #[test]
    fn constrained_hit_only_checks_the_original_button() {
        let starts = [0, 1, 3];

        assert_eq!(
            hit_index(spec(&starts, 4, 100), 20, 50.0, 10.0, Some(0)),
            None
        );
        assert_eq!(
            hit_index(spec(&starts, 4, 100), 20, 50.0, 10.0, Some(1)),
            Some(1)
        );
    }

    #[test]
    fn strip_region_width_scales_with_button_count() {
        let one = strip_region(1);
        let four = strip_region(4);

        assert_eq!(one.origin, STRIP_LEFT_MARGIN_PX);
        assert_eq!(one.width, STRIP_BUTTON_WIDTH_PX);
        assert_eq!(four.origin, STRIP_LEFT_MARGIN_PX);
        assert_eq!(four.width, 4 * STRIP_BUTTON_WIDTH_PX + 3 * STRIP_SPACING_PX);
    }

    #[test]
    fn controls_region_is_right_anchored_without_anchor() {
        let geo = controls_region(2, 2008, 0.0, None);

        let expected_width = 2 * CONTROL_UNIT_PX + CONTROL_SPACING_PX;
        assert_eq!(geo.width, expected_width);
        assert_eq!(
            geo.origin,
            2008.0 - CONTROL_RIGHT_MARGIN_PX - expected_width as f64
        );
    }

    #[test]
    fn controls_region_clamps_to_min_origin() {
        let min_origin = 300.0;
        let geo = controls_region(50, 2008, min_origin, None);

        assert_eq!(geo.origin, min_origin);
        assert_eq!(
            geo.width,
            (2008.0 - CONTROL_RIGHT_MARGIN_PX - min_origin) as i32
        );
    }

    #[test]
    fn anchored_region_sits_at_the_anchor_within_bounds() {
        let geo = controls_region(2, 2008, 300.0, Some(800.0));

        assert_eq!(geo.origin, 800.0);
        assert_eq!(geo.width, 2 * CONTROL_UNIT_PX + CONTROL_SPACING_PX);
    }

    #[test]
    fn anchor_clamps_to_min_origin_and_right_margin() {
        let expected_width = 2 * CONTROL_UNIT_PX + CONTROL_SPACING_PX;
        let max_origin = 2008.0 - CONTROL_RIGHT_MARGIN_PX - expected_width as f64;

        let left = controls_region(2, 2008, 300.0, Some(10.0));
        assert_eq!(left.origin, 300.0);

        let right = controls_region(2, 2008, 300.0, Some(5000.0));
        assert_eq!(right.origin, max_origin);
    }

    #[test]
    fn anchor_is_ignored_when_region_is_pinned_and_shrunk() {
        let geo = controls_region(50, 2008, 300.0, Some(700.0));

        assert_eq!(geo.origin, 300.0);
        assert_eq!(geo.width, (2008.0 - CONTROL_RIGHT_MARGIN_PX - 300.0) as i32);
    }

    #[test]
    fn non_finite_anchor_falls_back_to_right_anchored() {
        let expected_width = 2 * CONTROL_UNIT_PX + CONTROL_SPACING_PX;
        let max_origin = 2008.0 - CONTROL_RIGHT_MARGIN_PX - expected_width as f64;

        let geo = controls_region(2, 2008, 300.0, Some(f64::NAN));
        assert_eq!(geo.origin, max_origin);
    }
}
