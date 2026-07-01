use anyhow::{anyhow, Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

// Absolute-value sysfs backends for the brightness sliders (built-in panel +
// keyboard backlight). Write handles are opened while the daemon is still
// root and kept for the process lifetime — the same privdrop pattern as
// BacklightManager's bl_file. Reads go through the world-readable attr path
// and keep working after the drop to nobody.
//
// This code runs as root with Restart=always: it must never panic. A missing
// or unwritable device degrades to an inert slider, and write errors log
// once instead of spamming at drag rate.
pub struct SysfsSlider {
    write_handle: File,
    attr_path: PathBuf,
    max: u32,
    min_fraction: f64,
    write_error_logged: bool,
}

impl SysfsSlider {
    pub fn open(dir: &Path, min_fraction: f64) -> Result<SysfsSlider> {
        let attr_path = dir.join("brightness");
        let max = fs::read_to_string(dir.join("max_brightness"))
            .with_context(|| format!("reading {}/max_brightness", dir.display()))?
            .trim()
            .parse::<u32>()
            .with_context(|| format!("parsing {}/max_brightness", dir.display()))?;
        if max == 0 {
            return Err(anyhow!("{} reports max_brightness 0", dir.display()));
        }
        let write_handle = OpenOptions::new()
            .write(true)
            .open(&attr_path)
            .with_context(|| format!("opening {} for write", attr_path.display()))?;
        // A NaN min_fraction would become a NaN clamp *bound* in raw_for,
        // which panics — never let one in.
        let min_fraction = if min_fraction.is_finite() {
            min_fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Ok(SysfsSlider {
            write_handle,
            attr_path,
            max,
            min_fraction,
            write_error_logged: false,
        })
    }

    // Current brightness as a fraction of max; None if the attr is unreadable.
    pub fn read_value(&self) -> Option<f64> {
        let raw = fs::read_to_string(&self.attr_path)
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()?;
        Some((raw.min(self.max) as f64 / self.max as f64).clamp(0.0, 1.0))
    }

    pub fn write_value(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        let raw = self.raw_for(value);
        match self.write_handle.write_all(format!("{}\n", raw).as_bytes()) {
            Ok(()) => self.write_error_logged = false,
            Err(e) => {
                if !self.write_error_logged {
                    eprintln!("slider write to {} failed: {e}", self.attr_path.display());
                    self.write_error_logged = true;
                }
            }
        }
    }

    fn raw_for(&self, value: f64) -> u32 {
        (value.clamp(self.min_fraction, 1.0) * self.max as f64).round() as u32
    }
}

pub struct SliderBackends {
    pub display: Option<SysfsSlider>,
    pub keyboard: Option<SysfsSlider>,
}

impl SliderBackends {
    // Must be constructed before PrivDrop: opening the write handles needs
    // root, holding them does not.
    pub fn new(display_dir: &Path, kbd_dir: &Path) -> SliderBackends {
        let open =
            |dir: &Path, min_fraction: f64, label: &str| match SysfsSlider::open(dir, min_fraction)
            {
                Ok(slider) => Some(slider),
                Err(e) => {
                    eprintln!("{label} slider backend unavailable: {e:#}");
                    None
                }
            };
        SliderBackends {
            // Floor at 1% so a drag to the far left can never turn the panel
            // fully off with no visible way back.
            display: open(display_dir, 0.01, "display"),
            // Keyboard backlight 0 = off is desired.
            keyboard: open(kbd_dir, 0.0, "keyboard"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static DIR_SEQ: AtomicU32 = AtomicU32::new(0);

    fn fake_device(max: &str, current: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tiny-dfr-slider-test-{}-{}",
            std::process::id(),
            DIR_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("max_brightness"), max).unwrap();
        fs::write(dir.join("brightness"), current).unwrap();
        dir
    }

    fn read_raw(dir: &Path) -> String {
        // The writer reuses one fd without truncation, overwriting from the
        // current offset — after a single write the first line is the fresh
        // value (real sysfs attrs consume the whole write regardless).
        fs::read_to_string(dir.join("brightness"))
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string()
    }

    #[test]
    fn raw_value_round_trips_with_rounding_and_clamps() {
        let dir = fake_device("17777\n", "8887\n");
        let slider = SysfsSlider::open(&dir, 0.0).unwrap();

        assert_eq!(slider.raw_for(0.0), 0);
        assert_eq!(slider.raw_for(0.5), 8889); // round(0.5 * 17777)
        assert_eq!(slider.raw_for(1.0), 17777);
        assert_eq!(slider.raw_for(2.0), 17777);
        assert_eq!(slider.raw_for(-1.0), 0);
    }

    #[test]
    fn display_floor_never_writes_zero() {
        let dir = fake_device("17777\n", "8887\n");
        let mut slider = SysfsSlider::open(&dir, 0.01).unwrap();

        slider.write_value(0.0);

        assert_eq!(read_raw(&dir), "178"); // round(0.01 * 17777)
    }

    #[test]
    fn keyboard_backend_allows_zero() {
        let dir = fake_device("14660\n", "7000\n");
        let mut slider = SysfsSlider::open(&dir, 0.0).unwrap();

        slider.write_value(0.0);

        assert_eq!(read_raw(&dir), "0");
    }

    #[test]
    fn non_finite_values_are_dropped() {
        let dir = fake_device("100\n", "50\n");
        let mut slider = SysfsSlider::open(&dir, 0.0).unwrap();

        slider.write_value(f64::NAN);
        slider.write_value(f64::INFINITY);

        assert_eq!(read_raw(&dir), "50");
    }

    #[test]
    fn read_value_tolerates_trailing_newline_and_clamps() {
        let dir = fake_device("14660\n", "14660\n");
        let slider = SysfsSlider::open(&dir, 0.0).unwrap();

        assert_eq!(slider.read_value(), Some(1.0));

        fs::write(dir.join("brightness"), "0\n").unwrap();
        assert_eq!(slider.read_value(), Some(0.0));
    }

    #[test]
    fn zero_max_is_rejected() {
        let dir = fake_device("0\n", "0\n");

        assert!(SysfsSlider::open(&dir, 0.0).is_err());
    }

    #[test]
    fn missing_device_degrades_to_none() {
        let missing = std::env::temp_dir().join("tiny-dfr-slider-test-missing");
        let backends = SliderBackends::new(&missing, &missing);

        assert!(backends.display.is_none());
        assert!(backends.keyboard.is_none());
    }
}
