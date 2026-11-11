//! Omarchy theme colours derived from a wallpaper frame.

/// HSL with h in degrees, s and l in 0..1.
type Hsl = (f64, f64, f64);

fn to_hsl((r, g, b): (u8, u8, u8)) -> Hsl {
    let (r, g, b) = (f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let (l, d) = ((max + min) / 2.0, max - min);
    if d < 1e-9 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s.min(1.0), l)
}

fn hex((h, s, l): Hsl) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match h.rem_euclid(360.0) as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let byte = |v: f64| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
}

fn hsl_of_hex(hex_color: &str) -> Option<Hsl> {
    let h = hex_color.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    Some(to_hsl((byte(0)?, byte(2)?, byte(4)?)))
}

/// The most vivid colour of a palette as #rrggbb: what a wallpaper "is" at a glance.
pub fn vivid(palette: &[(f64, (u8, u8, u8))]) -> Option<String> {
    let total: f64 = palette.iter().map(|(n, _)| n).sum();
    let score = |(n, c): &(f64, (u8, u8, u8))| {
        let (_, s, l) = to_hsl(*c);
        s * (1.0 - (l - 0.5).abs()) * (n / total).powf(0.3)
    };
    let (_, c) = palette.iter().max_by(|a, b| score(a).total_cmp(&score(b)))?;
    Some(hex(to_hsl(*c)))
}

/// Whether a wallpaper colour sits with a theme accent: same hue family, or both near grey.
pub fn matches(wallpaper: &str, accent: &str) -> bool {
    let (Some(w), Some(a)) = (hsl_of_hex(wallpaper), hsl_of_hex(accent)) else { return false };
    if a.1 < 0.15 || w.1 < 0.15 {
        return a.1 < 0.15 && w.1 < 0.15;
    }
    ((w.0 - a.0 + 540.0).rem_euclid(360.0) - 180.0).abs() <= 45.0
}

/// `accent` from a theme's colors.toml.
pub fn accent(colors_toml: &str) -> Option<String> {
    let line = colors_toml.lines().find(|l| l.trim_start().starts_with("accent"))?;
    Some(line.split('"').nth(1)?.to_string())
}

/// Parse ImageMagick's `-format %c histogram:info:` output into (pixel count, colour).
pub fn parse_histogram(text: &str) -> Vec<(f64, (u8, u8, u8))> {
    text.lines()
        .filter_map(|line| {
            let count: f64 = line.split(':').next()?.trim().parse().ok()?;
            let hex = line.split('#').nth(1)?.get(..6)?;
            let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
            Some((count, (byte(0)?, byte(2)?, byte(4)?)))
        })
        .collect()
}

/// colors.toml for an Omarchy theme. `None` when the palette is empty.
///
/// The wallpaper decides the mood: surfaces take its dominant hue, the accent is its most
/// vivid colour. Terminal colours keep their meaning (red stays red) and only lean towards
/// the accent, so `git diff` and error output stay readable on any wallpaper.
pub fn colors_toml(palette: &[(f64, (u8, u8, u8))]) -> Option<String> {
    let total: f64 = palette.iter().map(|(n, _)| n).sum();
    if total <= 0.0 {
        return None;
    }
    let hsl: Vec<(f64, Hsl)> = palette.iter().map(|(n, c)| (n / total, to_hsl(*c))).collect();
    let light = hsl.iter().map(|(w, c)| w * c.2).sum::<f64>() > 0.6;
    let (_, base) = *hsl.iter().max_by(|a, b| a.0.total_cmp(&b.0))?;
    // Vivid and present: saturation counts most, a speck of a colour less than a patch.
    let score = |(w, (_, s, l)): &(f64, Hsl)| s * (1.0 - (l - 0.5).abs()) * w.powf(0.3);
    let (_, vivid) = *hsl.iter().max_by(|a, b| score(a).total_cmp(&score(b)))?;

    let tint = base.1.min(0.35);
    let surface = |l: f64| hex((base.0, tint * 0.7, l));
    let ink = |l: f64| hex((base.0, tint * 0.5, l));
    let accent_hsl = (vivid.0, vivid.1.clamp(0.45, 0.85), if light { 0.40 } else { 0.72 });
    // A fixed hue pulled 12% of the way towards the accent's.
    let term = |hue: f64| {
        let delta = (accent_hsl.0 - hue + 540.0).rem_euclid(360.0) - 180.0;
        hex((hue + delta * 0.12, accent_hsl.1.clamp(0.45, 0.7), if light { 0.40 } else { 0.72 }))
    };
    let l = |dark: f64, lite: f64| if light { lite } else { dark };

    let mut out = format!("mode = \"{}\"\n\n", if light { "light" } else { "dark" });
    let mut put = |key: &str, value: String| out += &format!("{key} = \"{value}\"\n");
    put("accent", hex(accent_hsl));
    put("selection", surface(l(0.27, 0.80)));
    put("muted", surface(l(0.38, 0.62)));
    put("background", surface(l(0.10, 0.95)));
    put("dark_background", surface(l(0.075, 0.91)));
    put("darker_background", surface(l(0.05, 0.87)));
    put("lighter_background", surface(l(0.19, 0.98)));
    put("foreground", ink(l(0.87, 0.18)));
    put("dark_foreground", ink(l(0.50, 0.48)));
    put("light_foreground", ink(l(0.78, 0.28)));
    put("bright_foreground", ink(l(0.92, 0.12)));
    let hues = [("red", 355.0), ("yellow", 45.0), ("orange", 25.0), ("green", 125.0), ("cyan", 175.0), ("blue", 215.0), ("magenta", 300.0)];
    for (name, hue) in hues {
        put(name, term(hue));
    }
    put("brown", hex((20.0, 0.25, l(0.42, 0.35))));
    for (name, hue) in hues.iter().filter(|(n, _)| *n != "orange") {
        put(&format!("bright_{name}"), term(*hue));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value<'a>(toml: &'a str, key: &str) -> &'a str {
        toml.lines().find_map(|l| l.strip_prefix(&format!("{key} = \""))).unwrap().trim_end_matches('"')
    }

    fn lightness(hex_color: &str) -> f64 {
        let b = |i| u8::from_str_radix(&hex_color[i..i + 2], 16).unwrap();
        to_hsl((b(1), b(3), b(5))).2
    }

    #[test]
    fn dark_purple_wallpaper_gives_dark_purple_theme() {
        let palette = parse_histogram(
            "      5120: (22,19,45) #16132D srgb(22,19,45)\n      2800: (40,30,90) #281E5A srgb(40,30,90)\n\
                    900: (120,80,255) #7850FF srgb(120,80,255)\n       396: (230,230,255) #E6E6FF srgb(230,230,255)\n",
        );
        assert_eq!(palette.len(), 4);
        let toml = colors_toml(&palette).unwrap();
        assert_eq!(value(&toml, "mode"), "dark");
        // Accent comes from the vivid violet, not from the dominant near-black.
        let accent = value(&toml, "accent");
        let b = |i| u8::from_str_radix(&accent[i..i + 2], 16).unwrap();
        let hue = to_hsl((b(1), b(3), b(5))).0;
        assert!((230.0..290.0).contains(&hue), "accent hue {hue}");
        // Text must stay readable on the background.
        assert!(lightness(value(&toml, "foreground")) - lightness(value(&toml, "background")) > 0.6);
        // Every key a stock theme has, each a #rrggbb.
        assert_eq!(toml.matches(" = \"#").count(), 25);
        assert!(lightness(value(&toml, "red")) > 0.5);
    }

    #[test]
    fn bright_wallpaper_gives_light_theme_and_empty_gives_none() {
        let toml = colors_toml(&[(10.0, (240, 240, 235)), (2.0, (200, 60, 40))]).unwrap();
        assert_eq!(value(&toml, "mode"), "light");
        assert!(lightness(value(&toml, "background")) - lightness(value(&toml, "foreground")) > 0.6);
        assert!(colors_toml(&[]).is_none());
        assert!(parse_histogram("garbage\n").is_empty());
        // Colour matching: hue family, wrap-around at red, and grey only with grey.
        assert!(matches("#7850ff", "#a197d8") && matches("#ff0010", "#ff1000"));
        assert!(!matches("#00ff00", "#a197d8") && !matches("#808080", "#a197d8") && matches("#808080", "#909090"));
        assert!(!matches("", "#a197d8"));
        assert_eq!(accent("mode = \"dark\"\n\naccent = \"#89b4fa\"\n").as_deref(), Some("#89b4fa"));
        assert_eq!(vivid(&[(10.0, (20, 20, 20)), (1.0, (255, 0, 0))]).as_deref(), Some("#ff0000"));
        // Round trip stays within rounding error.
        assert_eq!(hex(to_hsl((137, 180, 250))), "#89b4fa");
    }
}
