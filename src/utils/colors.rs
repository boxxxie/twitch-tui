use tui::style::Color::{self, Rgb};

use crate::config::Theme;

/// <https://css-tricks.com/converting-color-spaces-in-javascript/#hsl-to-rgb/>
pub fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> [u8; 3] {
    // Color intensity
    let chroma = (1. - 2.0f64.mul_add(lightness, -1.).abs()) * saturation;

    // Second largest component
    let x = chroma * (1. - ((hue / 60.) % 2. - 1.).abs());

    // Amount to match lightness
    let m = lightness - chroma / 2.;

    // Convert to rgb based on color wheel section
    let (mut red, mut green, mut blue) = match hue.round() as i32 {
        61..=120 => (x, chroma, 0.),
        121..=180 => (0., chroma, x),
        181..=240 => (0., x, chroma),
        241..=300 => (x, 0., chroma),
        301..=360 => (chroma, 0., x),
        _ => (chroma, x, 0.),
    };

    // Add amount to each channel to match lightness
    red = (red + m) * 255.;
    green = (green + m) * 255.;
    blue = (blue + m) * 255.;

    [red as u8, green as u8, blue as u8]
}

// Convert a u32 to RGB values, discarding the highest byte.
pub const fn u32_to_color(value: u32) -> Color {
    Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// Official 15 default Twitch chat username colors used when a user has not chosen a custom hex color.
pub const TWITCH_DEFAULT_COLORS: [[u8; 3]; 15] = [
    [0xFF, 0x00, 0x00], // Red
    [0x00, 0x00, 0xFF], // Blue
    [0x00, 0x80, 0x00], // Green
    [0xB2, 0x22, 0x22], // FireBrick
    [0xFF, 0x7F, 0x50], // Coral
    [0x9A, 0xCD, 0x32], // YellowGreen
    [0xFF, 0x45, 0x00], // OrangeRed
    [0x2E, 0x8B, 0x57], // SeaGreen
    [0xDA, 0xA5, 0x20], // GoldenRod
    [0xD2, 0x69, 0x1E], // Chocolate
    [0x5F, 0x9E, 0xA0], // CadetBlue
    [0x1E, 0x90, 0xFF], // DodgerBlue
    [0xFF, 0x69, 0xB4], // HotPink
    [0x8A, 0x2B, 0xE2], // BlueViolet
    [0x00, 0xFF, 0x7F], // SpringGreen
];

/// Parses a Twitch hex color string (e.g. `"#00FF7F"` or `"00FF7F"`) into a [`Color::Rgb`].
#[must_use]
pub fn parse_hex_color(hex: &str) -> Option<Color> {
    let clean = hex.trim().strip_prefix('#').unwrap_or_else(|| hex.trim());
    if clean.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&clean[0..2], 16).ok()?;
    let g = u8::from_str_radix(&clean[2..4], 16).ok()?;
    let b = u8::from_str_radix(&clean[4..6], 16).ok()?;
    Some(Rgb(r, g, b))
}

/// Deterministically selects one of Twitch's 15 default colors from a username.
#[must_use]
pub fn twitch_default_color(username: &str) -> Color {
    let sum = username
        .as_bytes()
        .iter()
        .fold(0usize, |acc, &b| acc.wrapping_add(usize::from(b.to_ascii_lowercase())));
    let [r, g, b] = TWITCH_DEFAULT_COLORS[sum % TWITCH_DEFAULT_COLORS.len()];
    Rgb(r, g, b)
}

#[must_use]
fn perceived_luminance(r: u8, g: u8, b: u8) -> f64 {
    0.0722f64.mul_add(
        f64::from(b),
        0.2126f64.mul_add(f64::from(r), 0.7152 * f64::from(g)),
    )
}

/// Ensures a username RGB color has sufficient contrast against the terminal background.
/// - On dark themes: black / near-black / dark gray colors are changed to white (`#F0F4F8`),
///   and dark chromatic colors (e.g. `#0000FF` pure blue or `#220044` dark purple) are lightened
///   toward white until they are easily readable.
/// - On light themes: white / near-white colors are changed to dark slate (`#1E293B`), and overly
///   bright colors are darkened for contrast.
#[must_use]
pub fn ensure_readable_color(color: Color, theme: &Theme) -> Color {
    let Rgb(r, g, b) = color else {
        return color;
    };

    let max_c = r.max(g).max(b);
    let min_c = r.min(g).min(b);
    let chroma = max_c.saturating_sub(min_c);
    let lum = perceived_luminance(r, g, b);

    match theme {
        Theme::Dark => {
            // Black, near-black, or dark achromatic gray -> clean white
            if max_c < 55 || (chroma < 24 && lum < 90.0) {
                return Rgb(0xF0, 0xF4, 0xF8);
            }
            // Dark chromatic color (like #0000FF blue, #1A0033 dark purple, #330000 dark red)
            let min_lum = 95.0;
            if lum < min_lum {
                // Blend proportionally toward white so hue stays recognizable
                let t = ((min_lum - lum) / (255.0 - lum)).clamp(0.25, 0.75);
                let lift = |c: u8| -> u8 {
                    let cf = f64::from(c);
                    cf.mul_add(1.0 - t, 255.0 * t).round() as u8
                };
                return Rgb(lift(r), lift(g), lift(b));
            }
            Rgb(r, g, b)
        }
        Theme::Light => {
            if min_c > 205 || (chroma < 24 && lum > 180.0) {
                return Rgb(0x1E, 0x29, 0x3B);
            }
            let max_lum = 165.0;
            if lum > max_lum {
                let scale = (max_lum / lum).clamp(0.35, 0.85);
                let darken = |c: u8| -> u8 { (f64::from(c) * scale).round() as u8 };
                return Rgb(darken(r), darken(g), darken(b));
            }
            Rgb(r, g, b)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_white_to_rgb() {
        let white_rgb = hsl_to_rgb(0.0, 0.0, 0.0);

        assert_eq!([0, 0, 0], white_rgb);
    }

    #[test]
    fn test_black_to_rgb() {
        let black_rgb = hsl_to_rgb(255.0, 255.0, 255.0);

        assert_eq!([255, 255, 0], black_rgb);
    }

    #[test]
    fn test_black_to_color() {
        let black_rgb = u32_to_color(u32::MIN);

        assert_eq!(Rgb(0, 0, 0), black_rgb);
    }

    #[test]
    fn test_u32_to_color() {
        let rgb = u32_to_color(126_496_416);

        assert_eq!(Rgb(138, 46, 160), rgb);
    }

    #[test]
    fn test_ensure_readable_color_dark_theme() {
        // Pure black and near-black become white
        assert_eq!(
            ensure_readable_color(Rgb(0, 0, 0), &Theme::Dark),
            Rgb(0xF0, 0xF4, 0xF8)
        );
        assert_eq!(
            ensure_readable_color(Rgb(20, 20, 25), &Theme::Dark),
            Rgb(0xF0, 0xF4, 0xF8)
        );

        // Pure dark blue (#0000FF, luminance ~18.4) gets lightened to a readable blue
        let readable_blue = ensure_readable_color(Rgb(0, 0, 255), &Theme::Dark);
        let Rgb(r, g, b) = readable_blue else {
            panic!("Expected Rgb");
        };
        assert!(perceived_luminance(r, g, b) >= 90.0);
        assert_eq!(b, 255);
        assert!(r > 60 && g > 60);
    }
}
