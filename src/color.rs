//! Turning a school's two colours into a readable report.
//!
//! Teachers give their school colours: a main colour and a second one. Rather
//! than painting text onto whatever they pick, every colour on the report has a
//! fixed job:
//!
//! - the **main colour** fills the header, footer and section banners, with
//!   black or white text, whichever reads better on it;
//! - the **second colour** is trim only: the thin stripes under the banners and
//!   the rules across the page. No text ever sits on it, so any colour works;
//! - **tints** for table headers, panels and striped rows are the main colour
//!   mixed mostly with white, so body text on them is always dark on light;
//! - headings use the main colour, darkened only as far as needed to read on
//!   the darkest tint.
//!
//! So blue and red gives blue banners with red trim and pale blue tables,
//! instead of pink tables or red text. Contrast follows WCAG 2.x.

use serde::Serialize;

type Rgb = [f64; 3];

pub const DEFAULT_MAIN: &str = "#003F5A";
pub const DEFAULT_TRIM: &str = "#B1D2E1";
const WHITE: Rgb = [1.0; 3];
const BLACK: Rgb = [0.0; 3];
/// Body text on the page.
const INK: &str = "#1A1A1A";
/// WCAG AA for normal text.
const AA: f64 = 4.5;
/// A main colour this close to white can't carry a banner (it would vanish
/// into the page), so the second colour leads instead, if it's deeper.
const TOO_PALE: f64 = 1.35;

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

/// Black or white, whichever is easier to read on `bg`. (With pure black, one
/// of the two always reaches 4.5:1.)
fn ink_on(bg: Rgb) -> Rgb {
    if contrast(bg, WHITE) >= contrast(bg, BLACK) { WHITE } else { BLACK }
}

/// `c`, darkened only as much as needed to read as text on `bg`.
fn readable_on(c: Rgb, bg: Rgb) -> Rgb {
    (0..=20).map(|i| mix(c, BLACK, i as f64 * 0.05)).find(|m| contrast(*m, bg) >= AA).unwrap_or(BLACK)
}

/// Every colour the report uses, as `#RRGGBB`.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Palette {
    /// Header, footer and section banners.
    pub banner: String,
    /// Text on the banners.
    pub on_banner: String,
    /// Stripes along the banners. Never has text on it.
    pub trim: String,
    /// Rules across the white page: the trim colour, unless it's too pale to
    /// show on white (then the main colour).
    pub rule: String,
    /// Headings and names on the page.
    pub heading: String,
    /// Secondary text (statuses, weights, notes).
    pub muted: String,
    /// Body text.
    pub ink: String,
    /// Table header rows: the darkest tint.
    pub head_fill: String,
    /// Unit rows inside a table.
    pub group_fill: String,
    /// Panels, striped rows and bar tracks.
    pub pale: String,
    /// Hairlines between rows and around boxes.
    pub line: String,
    /// True when the main colour was too pale for banners, so the two were
    /// swapped. Shown as a note in settings.
    pub swapped: bool,
}

/// The report palette for a school's main and second colours. Invalid input
/// falls back to the defaults.
pub fn palette(main: &str, second: &str) -> Palette {
    let mut m = parse(main).unwrap_or_else(|| parse(DEFAULT_MAIN).unwrap());
    let mut t = parse(second).unwrap_or_else(|| parse(DEFAULT_TRIM).unwrap());
    let swapped = contrast(m, WHITE) < TOO_PALE && contrast(t, WHITE) > contrast(m, WHITE);
    if swapped {
        std::mem::swap(&mut m, &mut t);
    }
    let tint = |share: f64| mix(WHITE, m, share);
    let head_fill = tint(0.16);
    let heading = readable_on(m, head_fill);
    // Grey leaning towards the main colour, readable on the darkest tint.
    let muted = readable_on(mix(heading, [0.42; 3], 0.6), head_fill);
    Palette {
        banner: hex(m),
        on_banner: hex(ink_on(m)),
        trim: hex(t),
        rule: hex(if contrast(t, WHITE) < TOO_PALE { m } else { t }),
        heading: hex(heading),
        muted: hex(muted),
        ink: INK.into(),
        head_fill: hex(head_fill),
        group_fill: hex(tint(0.10)),
        pale: hex(tint(0.05)),
        line: hex(tint(0.30)),
        swapped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: [&str; 14] = [
        "#003F5A", "#FFD400", "#B1D2E1", "#FFFFFF", "#000000", "#777777", "#E63946", "#2A9D8F", "#7FFF00", "#0033A0", "#C8102E", "#FF00FF",
        "#00FFFF", "#800000",
    ];

    #[test]
    fn every_text_colour_is_readable_for_any_pair() {
        for a in SAMPLES {
            for b in SAMPLES {
                let p = palette(a, b);
                let c = |h: &str| parse(h).unwrap();
                let fills = [&p.head_fill, &p.group_fill, &p.pale, "#FFFFFF"];
                for text in [&p.heading, &p.muted, &p.ink] {
                    for fill in fills {
                        assert!(contrast(c(text), c(fill)) >= AA, "{a}/{b}: {text} on {fill}");
                    }
                }
                assert!(contrast(c(&p.on_banner), c(&p.banner)) >= AA, "{a}/{b}: banner text");
            }
        }
    }

    #[test]
    fn blue_and_red_gives_blue_banners_with_red_trim() {
        let p = palette("#0033A0", "#C8102E");
        assert_eq!((p.banner.as_str(), p.on_banner.as_str(), p.trim.as_str()), ("#0033A0", "#FFFFFF", "#C8102E"));
        assert_eq!(p.heading, "#0033A0");
        // Tints come from the blue, not the red.
        let f = parse(&p.head_fill).unwrap();
        assert!(f[2] > f[0], "{} should be bluish", p.head_fill);
        assert!(!p.swapped);
    }

    #[test]
    fn a_white_main_colour_hands_the_banner_to_the_other() {
        let p = palette("#FFFFFF", "#800000");
        assert!(p.swapped);
        assert_eq!((p.banner.as_str(), p.trim.as_str(), p.rule.as_str()), ("#800000", "#FFFFFF", "#800000"));
        // Gold is pale but still a fine banner with black text.
        let g = palette("#FFD400", "#000000");
        assert!(!g.swapped);
        assert_eq!(g.on_banner, "#000000");
        assert_ne!(g.heading, "#FFD400");
    }

    #[test]
    fn bad_input_falls_back_to_the_defaults() {
        assert_eq!(palette("blue", ""), palette(DEFAULT_MAIN, DEFAULT_TRIM));
    }
}
