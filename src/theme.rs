//! Colour ramps.
//!
//! The visual difference from cmatrix lives here: cmatrix has three brightness
//! levels (white head / bold / normal), so its tails step. We map a continuous
//! `glow` in [0,1] onto a 24-bit ramp, which is what makes the trail read as a
//! fade rather than as bands.

use crate::rain::EPS;
use crossterm::style::Color;
use std::str::FromStr;
use thiserror::Error;

pub type Rgb = (u8, u8, u8);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ColorParseError {
    #[error("expected a colour name or #RRGGBB, got {0:?}")]
    Unrecognised(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    True,
    Ansi256,
    Ansi16,
}

impl Depth {
    /// Terminals advertise 24-bit support via `COLORTERM`; everything modern
    /// (iTerm2, kitty, WezTerm, Ghostty, Alacritty, VTE) sets it.
    #[must_use]
    pub fn detect() -> Depth {
        Depth::from_env(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    /// Split out from [`Depth::detect`] so the decision table is testable
    /// without mutating process-global environment.
    #[must_use]
    pub fn from_env(colorterm: Option<&str>, term: Option<&str>) -> Depth {
        match colorterm {
            Some("truecolor" | "24bit") => Depth::True,
            _ => match term {
                None | Some("" | "dumb") => Depth::Ansi16,
                Some(t) if t.contains("256") => Depth::Ansi256,
                // Assume 256 rather than 16: every terminal emulator in use
                // this decade has it, and guessing low looks noticeably worse.
                Some(_) => Depth::Ansi256,
            },
        }
    }

    #[must_use]
    pub fn to_color(self, (r, g, b): Rgb) -> Color {
        match self {
            Depth::True => Color::Rgb { r, g, b },
            Depth::Ansi256 => Color::AnsiValue(rgb_to_256(r, g, b)),
            Depth::Ansi16 => Color::AnsiValue(rgb_to_16(r, g, b)),
        }
    }
}

/// Brightness steps in the ramp. This is a *performance* control as much as a
/// visual one: a cell only becomes damaged when it crosses a step, so halving
/// the count roughly halves the escape sequences we hand the terminal. 24 is
/// already eight times cmatrix's three levels and reads as continuous.
pub const DEFAULT_LEVELS: u16 = 24;

/// Never quantise coarser than this — below it the tails visibly band.
pub const MIN_AUTO_LEVELS: u16 = 6;

/// Fitted so that `levels * sqrt(cells)` is roughly constant.
///
/// Output volume scales with cell count, while bytes-per-cell scales
/// *sublinearly* with the level count, so holding this product fixed keeps the
/// terminal's workload roughly flat as the window grows. The constant is set so
/// the curve passes through the two points we arrived at by measurement: 24 at a
/// stock 80x24, and 8 at a full-screen vertical 204x175.
const AUTO_LEVELS_K: f32 = 1500.0;

/// How many brightness steps to use for a window of `cells` cells.
#[must_use]
pub fn auto_levels(cells: usize) -> u16 {
    if cells == 0 {
        return DEFAULT_LEVELS;
    }
    let raw = AUTO_LEVELS_K / (cells as f32).sqrt();
    // Round rather than truncate: a full-screen vertical window lands on 7.94,
    // and truncating would quietly give 7 for a curve fitted to produce 8.
    raw.clamp(f32::from(MIN_AUTO_LEVELS), f32::from(DEFAULT_LEVELS))
        .round() as u16
}

/// The `--levels` setting: a fixed count, or sized from the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Levels {
    /// Recomputed from the terminal size, including after a resize.
    Auto,
    /// Exactly this many steps; 0 disables quantisation.
    Fixed(u16),
}

impl Levels {
    #[must_use]
    pub fn resolve(self, w: u16, h: u16) -> u16 {
        match self {
            Levels::Fixed(n) => n,
            Levels::Auto => auto_levels(w as usize * h as usize),
        }
    }
}

impl FromStr for Levels {
    type Err = LevelsParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("auto") {
            return Ok(Levels::Auto);
        }
        t.parse::<u16>()
            .map(Levels::Fixed)
            .map_err(|_| LevelsParseError(s.to_string()))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("expected \"auto\" or a number of brightness steps, got {0:?}")]
pub struct LevelsParseError(String);

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// Leading glyph — near-white, tinted toward the body hue.
    pub head: Rgb,
    /// Full-intensity body colour just behind the head.
    pub bright: Rgb,
    /// The colour a trail's brightness is measured up from: a cell at zero
    /// brightness would be this. Black by default.
    pub dim: Rgb,
    pub rainbow: bool,
    /// Quantisation steps; 0 disables quantisation entirely.
    pub levels: u16,
    /// How much dimmer a glyph starts on the bottom row than on the top, as a
    /// fraction of full brightness. See [`DEFAULT_DEPTH_FADE`].
    pub depth_fade: f32,
}

/// Glyphs on the bottom row start at three-quarters of the brightness they
/// would have on the top row, falling linearly in between.
///
/// In the film the rain is brightest near the top and a little subdued lower
/// down. A glyph never moves once written, so dimming by row is the same thing
/// as each drop's newest glyph starting dimmer the further it has fallen, and it
/// needs no state: it is a function of where the cell is. A quarter is enough to
/// read as depth without the bottom of a tall window looking unlit.
pub const DEFAULT_DEPTH_FADE: f32 = 0.25;

impl Theme {
    #[must_use]
    pub fn from_base(base: Rgb, rainbow: bool) -> Theme {
        Theme {
            head: head_tint(base),
            bright: base,
            dim: (0, 0, 0),
            rainbow,
            levels: DEFAULT_LEVELS,
            depth_fade: DEFAULT_DEPTH_FADE,
        }
    }

    /// The colour of a highlighted drop's cell on the top row. `glow` 1.0 is
    /// freshly written, 0.0 is about to be erased. `hue_shift` rotates the hue
    /// in rainbow mode and is ignored otherwise.
    #[must_use]
    pub fn color(&self, glow: f32, is_head: bool, hue_shift: f32) -> Rgb {
        self.color_at(glow, is_head, true, hue_shift, 0.0)
    }

    /// As [`color`](Self::color), for a cell written by a drop that is or isn't
    /// `highlighted`, `depth` of the way down the screen: 0.0 is the top row
    /// and 1.0 the bottom. Only a highlighted drop's head is drawn white. A
    /// highlighted trail holds full brightness for its first half and slides
    /// to half over the second; an ordinary one runs at half throughout. Both
    /// dim over their last 15% and never go below a quarter while lit.
    #[must_use]
    pub fn color_at(
        &self,
        glow: f32,
        is_head: bool,
        highlighted: bool,
        hue_shift: f32,
        depth: f32,
    ) -> Rgb {
        let depth = if depth.is_finite() {
            depth.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let peak = 1.0 - self.depth_fade.clamp(0.0, 1.0) * depth;
        let (bright, dim, head) = if self.rainbow {
            let b = hsv_to_rgb(hue_shift.rem_euclid(1.0), 0.85, 1.0);
            (b, self.dim, head_tint(b))
        } else {
            (self.bright, self.dim, self.head)
        };

        if is_head && highlighted {
            return lerp(dim, head, peak);
        }
        let t = if glow.is_finite() {
            glow.clamp(0.0, 1.0)
        } else {
            0.0
        };
        // Quantise *after* the profile so the steps are evenly spaced in the
        // output colour rather than bunched at the dim end, and floor *after*
        // the depth fade so the bottom rows can't sink under it.
        let level = self.quantise(trail_brightness(t, highlighted)) * peak;
        let level = if level > 0.0 { level.max(DIMMEST) } else { 0.0 };
        lerp(dim, bright, level)
    }

    /// Snap up to the next of `levels` steps. Holding a cell's colour steady
    /// across frames is what stops it generating damage every single frame.
    ///
    /// Up, not to nearest: a lit cell never rounds down to black. Drawing a
    /// black glyph costs the same bytes as a visible one and shows as a smudge
    /// on any background that isn't pure black.
    fn quantise(&self, t: f32) -> f32 {
        if self.levels == 0 || t <= 0.0 {
            return t.max(0.0);
        }
        let n = f32::from(self.levels);
        (t * n).ceil() / n
    }
}

/// Brightness of an ordinary drop's whole trail, and where a highlighted
/// drop's trail settles after its bright first half.
///
/// Half the colour value, which on a black background is also about half the
/// perceived lightness (L* 46 against 88 for the default green). In the film
/// most strings sit at this one level and only the highlighted ones run
/// brighter.
const SETTLED: f32 = 0.5;

/// How far along its trail a highlighted drop stays at full brightness.
const HOLD_UNTIL: f32 = 0.5;

/// Where every trail starts its fade to black.
///
/// The film cuts glyphs off; here they fade, because a cut from 50% straight to
/// black is the jump this profile exists to avoid. 15% is about one row on the
/// shortest default trail and four on the longest — enough to read as a fade
/// on most drops without eating into the settled body.
const TAIL_FROM: f32 = 0.85;

/// The dimmest a lit glyph is ever drawn, as a fraction of the base colour, so
/// a trail fades to dark green and is then erased.
///
/// Judged by eye in Ghostty on a black background: at 0.125 (green 32) the end
/// of every trail read as a scatter of black shapes that caught the eye, and at
/// 0.25 (green 64) it reads as the dim end of the trail. It is a fixed level on
/// purpose. Flooring at a quantisation step instead sank with the step count —
/// green 21 at the 24 levels `auto` picks for a small window — and the depth
/// fade took another quarter off the bottom rows.
const DIMMEST: f32 = 0.25;

/// Brightness in [0,1] for a trail cell, falling to exactly 0 at the glow where
/// `Rain` erases it.
///
/// Glow decays exponentially, and every drop's decay is solved so that glow
/// reaches `EPS` exactly `tail` rows behind the head. So `ln(glow) / ln(EPS)`
/// is the cell's position along its own trail, 0 at the head and 1 at the
/// erase, whatever the drop's speed and length. The profile is a function of
/// that position:
///
/// | along | highlighted drop | ordinary drop |
/// |---|---|---|
/// | 0 to 0.5 | full | half |
/// | 0.5 to 0.85 | full sliding to half | half |
/// | 0.85 to 1 | half sliding to black | half sliding to black |
///
/// The tail runs all the way to 0 so the descent is even right down to
/// [`DIMMEST`], where `color_at` holds it until the erase. The ramp this
/// replaced flattened out at 18% halfway down every trail and sat there, so
/// the back half of each trail was one colour ending in a cut.
fn trail_brightness(glow: f32, highlighted: bool) -> f32 {
    if glow <= EPS {
        return 0.0;
    }
    let along = (glow.ln() / EPS.ln()).clamp(0.0, 1.0);
    let body = if highlighted {
        let settling = ((along - HOLD_UNTIL) / (TAIL_FROM - HOLD_UNTIL)).clamp(0.0, 1.0);
        1.0 - (1.0 - SETTLED) * settling
    } else {
        SETTLED
    };
    let tail = ((along - TAIL_FROM) / (1.0 - TAIL_FROM)).clamp(0.0, 1.0);
    body * (1.0 - tail)
}

fn head_tint(base: Rgb) -> Rgb {
    lerp(base, (255, 255, 255), 0.80)
}

fn lerp((r1, g1, b1): Rgb, (r2, g2, b2): Rgb, t: f32) -> Rgb {
    let f = |a: u8, b: u8| {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (f(r1, r2), f(g1, g2), f(b1, b2))
}

#[must_use]
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Rgb {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// A parsed base colour plus whether the user asked for rainbow mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BaseColor(pub Rgb, pub bool);

impl FromStr for BaseColor {
    type Err = ColorParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = s.trim().to_ascii_lowercase();
        let rgb = match key.as_str() {
            "green" => (0, 255, 65), // the canonical Matrix green
            "red" => (255, 40, 40),
            "blue" => (60, 120, 255),
            "cyan" => (0, 230, 230),
            "magenta" => (230, 60, 230),
            "yellow" => (240, 220, 60),
            "orange" => (255, 140, 30),
            "purple" => (170, 90, 255),
            "white" => (220, 220, 220),
            "rainbow" => return Ok(BaseColor((0, 255, 65), true)),
            hex => {
                let hex = hex.strip_prefix('#').unwrap_or(hex);
                if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(ColorParseError::Unrecognised(s.to_string()));
                }
                // Safe: the guard above proved all six chars are hex digits.
                let p = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
                (p(0), p(2), p(4))
            }
        };
        Ok(BaseColor(rgb, false))
    }
}

fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    // Greys have their own ramp and quantise much better there.
    if r.abs_diff(g) < 8 && g.abs_diff(b) < 8 {
        let level = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
        if level < 8 {
            return 16;
        }
        if level > 248 {
            return 231;
        }
        // The grey ramp is 232..=255 — 24 slots, so the index must top out at
        // 23. Scaling by 24 here reaches 24 at level 248 and overflows u8.
        return 232 + ((level - 8) * 23 / 240) as u8;
    }
    let q = |c: u8| u16::from(c) * 5 / 255;
    (16 + 36 * q(r) + 6 * q(g) + q(b)) as u8
}

fn rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    let bright = r.max(g).max(b) > 128;
    let bit = |c: u8| u8::from(c > 96);
    let base = bit(r) | (bit(g) << 1) | (bit(b) << 2);
    if bright { base + 8 } else { base }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luma((r, g, b): Rgb) -> f32 {
        0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)
    }

    #[test]
    fn named_colors_parse() {
        assert_eq!(
            BaseColor::from_str("green"),
            Ok(BaseColor((0, 255, 65), false))
        );
        assert_eq!(
            BaseColor::from_str("  GREEN  "),
            Ok(BaseColor((0, 255, 65), false))
        );
        assert_eq!(
            BaseColor::from_str("rainbow"),
            Ok(BaseColor((0, 255, 65), true))
        );
    }

    #[test]
    fn hex_colors_parse_with_and_without_hash() {
        assert_eq!(
            BaseColor::from_str("#ff8800"),
            Ok(BaseColor((255, 136, 0), false))
        );
        assert_eq!(
            BaseColor::from_str("FF8800"),
            Ok(BaseColor((255, 136, 0), false))
        );
    }

    #[test]
    fn bad_colors_are_rejected_not_panicked() {
        for bad in ["", "#ff88", "#gggggg", "chartreuse", "#ff88000", "##ffffff"] {
            assert!(
                BaseColor::from_str(bad).is_err(),
                "{bad:?} should not parse"
            );
        }
    }

    #[test]
    fn ramp_is_monotonic_in_glow() {
        let t = Theme::from_base((0, 255, 65), false);
        let mut prev = -1.0;
        for i in 0..=20 {
            let l = luma(t.color(i as f32 / 20.0, false, 0.0));
            assert!(l >= prev, "ramp dipped at glow {}", i as f32 / 20.0);
            prev = l;
        }
    }

    #[test]
    fn a_trail_steps_down_evenly_to_dark_green_then_is_erased() {
        // The original ramp stalled at 18% for the back half of every trail and
        // was then cut to black. Now a trail descends in even steps, holds at
        // DIMMEST, and the erase is the only step bigger than the rest.
        let floor = i32::from(lerp((0, 0, 0), (0, 255, 65), DIMMEST).1);
        for (levels, highlighted) in [0u16, 6, 8, 12, 24]
            .into_iter()
            .flat_map(|l| [(l, true), (l, false)])
        {
            let mut t = Theme::from_base((0, 255, 65), false);
            t.levels = levels;
            // Head to erasure exactly as `Rain` decays a cell, then the erased
            // cell itself, which the renderer blanks to the background.
            let mut greens: Vec<i32> = (0..=2000)
                .map(|i| EPS.powf(i as f32 / 2000.0))
                .map(|glow| i32::from(t.color_at(glow, false, highlighted, 0.0, 0.0).1))
                .collect();
            greens.push(0);
            greens.dedup();
            let steps: Vec<i32> = greens.windows(2).map(|w| w[0] - w[1]).collect();
            let (last, rest) = steps.split_last().expect("the ramp has steps");
            assert_eq!(
                *last, floor,
                "levels {levels}: erased from {last}, not dark green"
            );
            // One quantisation step, or a rounding unit when unquantised.
            let even = if levels == 0 {
                2
            } else {
                255 / i32::from(levels) + 2
            };
            assert!(
                rest.iter().all(|&s| s <= even),
                "levels {levels}, highlighted {highlighted}: uneven descent {rest:?}"
            );
        }
    }

    #[test]
    fn a_lit_glyph_is_never_dimmer_than_dark_green() {
        // Glyphs much dimmer than this read as black shapes rather than green,
        // and every trail ended in them. Two things pushed tails under it: the
        // floor was a quantisation step, so it sank as `auto` picked more
        // levels (green 21 at 24 levels), and the depth fade scaled it down a
        // further quarter on the bottom rows (27 seen at 14 levels).
        let floor = lerp((0, 0, 0), (0, 255, 65), DIMMEST).1;
        for levels in [0u16, 6, 8, 14, 24] {
            for highlighted in [true, false] {
                for depth in [0.0, 0.5, 1.0] {
                    let mut t = Theme::from_base((0, 255, 65), false);
                    t.levels = levels;
                    let dimmest = (0..2000)
                        .map(|i| EPS.powf(i as f32 / 2000.0))
                        .map(|glow| t.color_at(glow, false, highlighted, 0.0, depth).1)
                        .filter(|&g| g > 0)
                        .min();
                    assert_eq!(
                        dimmest,
                        Some(floor),
                        "levels {levels}, highlighted {highlighted}, depth {depth}"
                    );
                }
            }
        }
    }

    /// Brightness `along` of the way down a trail, before quantisation and the
    /// DIMMEST floor, so the profile itself is what is measured.
    fn profile(along: f32, highlighted: bool) -> f32 {
        trail_brightness(EPS.powf(along), highlighted)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn a_highlighted_trail_holds_full_brightness_for_half_its_length_then_settles_at_half() {
        for along in [0.0, 0.2, 0.45, 0.5] {
            assert!(
                close(profile(along, true), 1.0),
                "{along}: {}",
                profile(along, true)
            );
        }
        // Sliding, not stepping, between the hold and the settled level.
        assert!(
            close(profile(0.675, true), 0.75),
            "{}",
            profile(0.675, true)
        );
        assert!(close(profile(0.85, true), 0.5), "{}", profile(0.85, true));
    }

    #[test]
    fn an_ordinary_trail_sits_at_half_brightness_until_its_tail() {
        // Four strings in five run their whole length at one dim level in the
        // film; fading from the head made every drop look like a highlighted
        // one, only shorter.
        for along in [0.0, 0.3, 0.6, 0.85] {
            assert!(
                close(profile(along, false), 0.5),
                "{along}: {}",
                profile(along, false)
            );
        }
        let mut t = Theme::from_base((0, 255, 65), false);
        t.levels = 0;
        assert_eq!(
            t.color_at(1.0, true, false, 0.0, 0.0),
            t.color_at(1.0, false, false, 0.0, 0.0),
            "an ordinary head is the same green as its trail"
        );
    }

    #[test]
    fn every_trail_fades_to_black_over_its_last_stretch() {
        for highlighted in [true, false] {
            assert!(close(profile(0.925, highlighted), 0.25));
            assert!(profile(0.99, highlighted) < 0.05);
            assert!(close(profile(1.0, highlighted), 0.0));
        }
    }

    #[test]
    fn glyphs_start_dimmer_toward_the_bottom_of_the_screen() {
        let t = Theme::from_base((0, 255, 65), false);
        for is_head in [false, true] {
            let at = |depth: f32| luma(t.color_at(1.0, is_head, true, 0.0, depth));
            let ratio = at(1.0) / at(0.0);
            assert!(
                (0.73..=0.77).contains(&ratio),
                "head {is_head}: the bottom row is {ratio} of the top"
            );
            for i in 0..10 {
                let (upper, lower) = (i as f32 / 10.0, (i + 1) as f32 / 10.0);
                assert!(
                    at(lower) <= at(upper),
                    "head {is_head}: brighter at {lower}"
                );
            }
        }
        // And with the fade off, every row is drawn alike.
        let flat = Theme {
            depth_fade: 0.0,
            ..t
        };
        assert_eq!(
            flat.color_at(0.5, false, true, 0.0, 0.0),
            flat.color_at(0.5, false, true, 0.0, 1.0)
        );
        assert_eq!(
            t.color_at(0.5, false, true, 0.0, f32::NAN),
            t.color(0.5, false, 0.0)
        );
    }

    #[test]
    fn head_is_the_brightest_thing_on_screen() {
        let t = Theme::from_base((0, 255, 65), false);
        assert!(luma(t.color(1.0, true, 0.0)) > luma(t.color(1.0, false, 0.0)));
        assert!(luma(t.color(1.0, false, 0.0)) > luma(t.color(0.0, false, 0.0)));
    }

    #[test]
    fn out_of_range_glow_is_clamped() {
        let t = Theme::from_base((0, 255, 65), false);
        assert_eq!(t.color(5.0, false, 0.0), t.color(1.0, false, 0.0));
        assert_eq!(t.color(-5.0, false, 0.0), t.color(0.0, false, 0.0));
        assert_eq!(t.color(f32::NAN, false, 0.0), t.color(0.0, false, 0.0));
    }

    #[test]
    fn rainbow_hue_rotates_and_wraps() {
        let t = Theme::from_base((0, 255, 65), true);
        assert_ne!(t.color(1.0, false, 0.0), t.color(1.0, false, 0.33));
        // Negative and >1 shifts must wrap rather than saturate.
        assert_eq!(t.color(1.0, false, 0.25), t.color(1.0, false, 1.25));
        assert_eq!(t.color(1.0, false, 0.25), t.color(1.0, false, -0.75));
    }

    #[test]
    fn depth_detection_table() {
        assert_eq!(
            Depth::from_env(Some("truecolor"), Some("xterm")),
            Depth::True
        );
        assert_eq!(Depth::from_env(Some("24bit"), Some("xterm")), Depth::True);
        assert_eq!(
            Depth::from_env(None, Some("xterm-256color")),
            Depth::Ansi256
        );
        assert_eq!(Depth::from_env(None, Some("xterm")), Depth::Ansi256);
        assert_eq!(Depth::from_env(None, Some("dumb")), Depth::Ansi16);
        assert_eq!(Depth::from_env(None, None), Depth::Ansi16);
        assert_eq!(
            Depth::from_env(Some("something-else"), Some("")),
            Depth::Ansi16
        );
    }

    #[test]
    fn every_depth_maps_every_color_without_panicking() {
        for depth in [Depth::True, Depth::Ansi256, Depth::Ansi16] {
            for c in [
                (0, 0, 0),
                (255, 255, 255),
                (0, 255, 65),
                (7, 7, 7),
                (250, 250, 250),
            ] {
                let _ = depth.to_color(c);
            }
        }
    }

    #[test]
    fn greyscale_quantisation_stays_in_the_256_ramp() {
        for level in 0..=255u8 {
            let v = rgb_to_256(level, level, level);
            assert!(
                v == 16 || v == 231 || (232..=255).contains(&v),
                "grey {level} -> {v}"
            );
        }
    }

    #[test]
    fn auto_levels_matches_the_sizes_we_tuned_by_hand() {
        // These two are the anchors the curve was fitted to; if either moves,
        // the constant is wrong.
        assert_eq!(
            auto_levels(80 * 24),
            24,
            "a stock terminal should stay at the default"
        );
        assert_eq!(
            auto_levels(204 * 175),
            8,
            "full-screen vertical should land on 8"
        );
        // And the middle should land between, monotonically.
        let mid = auto_levels(200 * 50);
        assert!((10..=18).contains(&mid), "200x50 picked {mid}");
    }

    #[test]
    fn auto_levels_is_monotonic_and_bounded() {
        let mut prev = u16::MAX;
        for cells in [1, 500, 1920, 4800, 10_000, 35_700, 100_000, 5_000_000] {
            let l = auto_levels(cells);
            assert!(l <= prev, "levels rose with cell count at {cells}");
            assert!(
                (MIN_AUTO_LEVELS..=DEFAULT_LEVELS).contains(&l),
                "{cells} cells gave {l}, outside the clamp"
            );
            prev = l;
        }
    }

    #[test]
    fn auto_levels_handles_a_zero_sized_window() {
        assert_eq!(auto_levels(0), DEFAULT_LEVELS);
    }

    #[test]
    fn levels_setting_parses() {
        assert_eq!(Levels::from_str("auto"), Ok(Levels::Auto));
        assert_eq!(Levels::from_str("  AUTO "), Ok(Levels::Auto));
        assert_eq!(Levels::from_str("12"), Ok(Levels::Fixed(12)));
        // 0 is the documented "no quantisation" escape hatch.
        assert_eq!(Levels::from_str("0"), Ok(Levels::Fixed(0)));
        for bad in ["", "-1", "auto8", "twelve", "1.5"] {
            assert!(Levels::from_str(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn fixed_levels_ignore_the_window_but_auto_tracks_it() {
        assert_eq!(Levels::Fixed(9).resolve(80, 24), 9);
        assert_eq!(Levels::Fixed(9).resolve(204, 175), 9);
        // Auto must actually change when the window does — this is what makes
        // a resize adapt rather than keep a stale setting.
        assert_ne!(Levels::Auto.resolve(80, 24), Levels::Auto.resolve(204, 175));
        assert_eq!(Levels::Auto.resolve(204, 175), auto_levels(204 * 175));
    }

    #[test]
    fn hsv_primaries() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), (255, 0, 0));
        assert_eq!(hsv_to_rgb(1.0 / 3.0, 1.0, 1.0), (0, 255, 0));
        assert_eq!(hsv_to_rgb(2.0 / 3.0, 1.0, 1.0), (0, 0, 255));
        assert_eq!(hsv_to_rgb(0.0, 0.0, 1.0), (255, 255, 255));
    }
}
