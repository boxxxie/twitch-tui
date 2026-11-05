use tui::style::Color::{self, Rgb};

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
}
