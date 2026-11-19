//! Keeping the school colours on PDF reports readable.
//!
//! A teacher can choose any main colour for their reports. Text on it (the
//! header, footer and section banners) is black or white, whichever contrasts
//! more, and the colour itself is darkened where it's used as text on the white
//! page. Contrast follows WCAG 2.x relative luminance.

type Rgb = [f64; 3];

const DEFAULT_PRIMARY: &str = "#003F5A";
/// Text colours used on top of a brand colour.
const LIGHT_INK: &str = "#FFFFFF";
// Pure black: with it, black or white always reaches 4.5:1 on any colour.
const DARK_INK: &str = "#000000";
/// The report page (white, with pale tinted panels: check against a light grey).
const LIGHT_PAGE: &str = "#F3F5F7";
/// WCAG AA for normal text.
const AA: f64 = 4.5;

fn parse(hex: &str) -> Option<Rgb> {
    if !crate::engine::is_hex_color(hex) {
        return None;
    }
    let ch = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|v| v as f64 / 255.0);
    Some([ch(1)?, ch(3)?, ch(5)?])
}

fn hex(c: Rgb) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", b(c[0]), b(c[1]), b(c[2]))
}

fn luminance(c: Rgb) -> f64 {
    let lin = |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `a` with `t` (0–1) of `b` mixed in, like CSS color-mix(in srgb, a, b t).
fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn rgb(hex: &str) -> Rgb {
    parse(hex).expect("built-in colour")
}

/// Black or white, whichever is easier to read on `bg`.
fn ink_on(bg: Rgb) -> &'static str {
    if contrast(bg, rgb(LIGHT_INK)) >= contrast(bg, rgb(DARK_INK)) { LIGHT_INK } else { DARK_INK }
}

/// `c`, darkened only as much as needed to read as text on `page`.
fn readable_on(c: Rgb, page: Rgb) -> Rgb {
    (0..=20).map(|i| mix(c, [0.0; 3], i as f64 * 0.05)).find(|m| contrast(*m, page) >= AA).unwrap_or([0.0; 3])
}

/// Colours derived from a teacher's main report colour.
pub struct Brand {
    /// Text drawn on the main colour.
    pub on_primary: String,
    /// The main colour as text on a white or light-grey page.
    pub text: String,
}

pub fn brand(primary: &str) -> Brand {
    let p = parse(primary).unwrap_or_else(|| rgb(DEFAULT_PRIMARY));
    Brand { on_primary: ink_on(p).into(), text: hex(readable_on(p, rgb(LIGHT_PAGE))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_on_the_brand_colour_is_always_readable() {
        for c in ["#003F5A", "#FFD400", "#B1D2E1", "#FFFFFF", "#000000", "#777777", "#E63946", "#2A9D8F", "#7FFF00"] {
            let b = brand(c);
            assert!(contrast(rgb(c), rgb(&b.on_primary)) >= AA, "{c} with {}", b.on_primary);
            assert!(contrast(rgb(&b.text), rgb(LIGHT_PAGE)) >= AA, "{c} as text: {}", b.text);
            assert!(contrast(rgb(&b.text), [1.0; 3]) >= AA, "{c} as text on white: {}", b.text);
        }
    }

    #[test]
    fn dark_colours_are_left_alone() {
        let b = brand("#003f5a");
        assert_eq!((b.on_primary.as_str(), b.text.as_str()), ("#FFFFFF", "#003F5A"));
        let y = brand("#FFD400");
        assert_eq!(y.on_primary, DARK_INK);
        assert_ne!(y.text, "#FFD400");
    }
}
