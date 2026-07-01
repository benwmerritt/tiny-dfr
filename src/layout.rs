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
}
