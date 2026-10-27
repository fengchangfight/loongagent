//! Purpose: The two pictures the screen is built from — the block-letter wordmark and the favicon at its two sizes — as text.
//!
//! # Contract
//! - **Serves:** `App::draw_banner` and `App::draw_splash` in `crates/tui-app/src/lib.rs`; `tests.rs` pins what they draw.
//! - **Design:** `docs/design/surfaces.md` §3 ("The TUI's marks and colour").
//! - **Upstream:** `lib.rs` names this module; nothing else reaches it.
//! - **Downstream:** `ratatui`'s `Line`, `Span` and `Color`, and `unicode_width` for centring.
//! - **Verify:** `cargo test -p loong-tui-app`.
//!
//! Both pictures are written as text so a change to them is reviewable in a diff. The
//! wordmark is five rows of blocks. The favicon is a pixel grid: one character per pixel,
//! two pixel rows to one screen row, drawn with a half block — the upper pixel becomes the
//! glyph's colour, the lower its background. A pixel the picture does not cover stays the
//! terminal's own background, so the dragon sits on whatever theme the person runs.
//!
//! The inks are `Red`, `LightRed`, `Yellow`, `LightYellow` and `Black`, all named: a fixed
//! RGB would be chosen for one background and unreadable on the other, which is the rule
//! the whole surface follows (`docs/design/surfaces.md` §3).

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// The name, five rows of five-by-five blocks with one column between letters: ten letters
/// is 59 columns. Uppercase, because a lowercase `g` has a descender and five rows have no
/// room for one.
pub const WORDMARK: [&str; 5] = [
    "█     █████ █████ █   █ █████ █████ █████ █████ █   █ █████",
    "█     █   █ █   █ ██  █ █     █   █ █     █     ██  █   █",
    "█     █   █ █   █ █ █ █ █ ███ █████ █ ███ ████  █ █ █   █",
    "█     █   █ █   █ █  ██ █   █ █   █ █   █ █     █  ██   █",
    "█████ █████ █████ █   █ █████ █   █ █████ █████ █   █   █",
];

/// How wide the wordmark is, so the header can decide whether it fits before drawing it.
pub const WORDMARK_WIDTH: usize = 59;

/// The favicon at full size: 40 columns and 52 pixel rows, in the terminal's named inks.
///
/// Each character is one pixel: `r` red, `R` light red, `y` gold, `Y` pale gold, `k` the
/// dark outline and the code panel, and a space whatever is behind the dragon. It is a
/// downsample of `favicon.png`, quantised to those five inks by hand; at this size the
/// horns, the eye, the coiled body and the panel inside it are all still legible.
pub const FAVICON_WIDE: [&str; 52] = [
    "               Yy    Yy                 ",
    "               Yy   YY                  ",
    "              Yy    Yy                  ",
    "              Y    yy   YYY             ",
    "             yR kyyR      YYY           ",
    "             yrrryR  Rr     YYY         ",
    "            Rrrryr rrrrRRRR   Yy        ",
    "       YY  rrrrrrrkkrrrrrrr    yy       ",
    "      YY   rrrrrrkrrrrrrrrrrrR   R      ",
    "     YY   rrRrrrrrrrrrrrrRRrrr    R     ",
    "     Y   rrRrrrrrrrkrrrrrrrrrrR   RR    ",
    "    YY   rrrrrrrrrrRrrrrrrrrrrrR   RR   ",
    "   YY    rrrrrrrRRRrrrrRRRrrrrrr    R   ",
    "   Y     RrrrrrryRrr   rrRRrrrrr     r  ",
    "  YY   RRrrrrrRyyr      krRRrrrrr    rr ",
    "  Y  rrrrrrrrRyyR        kRRrrrrrr    r ",
    " YY  rrrrrrrRRrrr        krRRrrrrr    r ",
    " Y    rrrrrr             kkRRrrrr     rr",
    "YY    rrrrr             rkrRRrrrk      R",
    "YY     rrr      rrrkkkrkkkrRrrrrk   y  R",
    "Y               kkkrrrrkkrrrrrrkk   yy R",
    "Y            rrrrrrrrrrrrrrrkkkkkkk yyyR",
    "Y         RyrrrrrrrrrrrrrkRrrrrrrrk yyyR",
    "Y         rrrrrrrrrrrrrrkkRkkkkkrrk yyyy",
    "Y      RRRrrrrrrrrrrrrrkrkRkkrkkkrk YYYY",
    "Y     rrrrrrrrrRrrrrrrrkkkRkkrkkkRk YYRY",
    "Y      rrrrrRRRRrrrkrrkkkkrkykkrkRk YyrR",
    "Y     rrkrrRRRRRrkkkkkkkkkrkkrrkkrk y r ",
    "Y    rrkrrRRRRRrkkkkkkkkkkrkkrkkkrk   R ",
    "Y   rrrrrRRRRRrkkrRrrrrrrkrkkkkkrrk   Rr",
    "Y   rrrrrRRyyr kkrrkkkrkrkrrrrrrrkk   Rr",
    "Y  rrrrrrRRyRr kkrrkkykkrkkrrrrkkkk   rr",
    "YY rrrrrRRRyR  kkrrkrrkkrkrkkkkrk     rr",
    "YY r rrrRRyyr  kkrrkykkkrkrkkkkrk    Rrr",
    " Y   rrrRRRRr  kkrrykrrrrkrkrkkrk    Rr ",
    " Yy  rrrRRRRr  kkrrykkkRrkrkkrkrk    rr ",
    " yy  rrrRRRRr  kkrrkkkrkrkrkkkkrk   Rrr ",
    "  R  rrrRRRRrR kkrrrrrrrRkrrrrrrk  Rrrr ",
    "  RR rkrrRRRrr kkkkkkkkkkkkkkkkkk  Rrr  ",
    "   R   rrrRRrr  kkkrrrkkkkkkkkkk  Rrrr  ",
    "   RR  krrRRrrRy  rkkkrk         Rrrr   ",
    "    RR  rrrrrrrr  rkrkrk        Rrrr    ",
    "     rr krrrrrrr  rkkkrk      yRrrrR    ",
    "      rr krrrrrrrRkkkkkk    RRRrrrR     ",
    "      rrr krrrrrrRRrrkkr RRRRrrrry      ",
    "       rrr krrrrrrrrRRRRRRrrrrrRY       ",
    "         rr  rrrrrrrrrrrrrrrrryY        ",
    "          rrr  krrrrrrrrrrrrrRR         ",
    "            rrrr  kkkkkkrrrRR           ",
    "              rrrrrrrrrrrrr             ",
    "                  rrrrr                 ",
    "                                        ",
];

/// How many columns [`FAVICON_WIDE`] draws.
pub const FAVICON_WIDE_COLUMNS: usize = 40;

/// The favicon where the screen is 92 columns or more but not 112: 34 columns and 40 pixel
/// rows. Same picture, fewer pixels — the horns and the coil survive, the panel's glyphs do
/// not, which is the trade a smaller screen forces.
pub const FAVICON_NARROW: [&str; 40] = [
    "             Y    Y               ",
    "            Yy   Yy               ",
    "            y   yy  YY            ",
    "           yR ryR     YYY         ",
    "          Rr ryR rr yy  YYY       ",
    "      YY  rrrRrkkrrrrr    yy      ",
    "     YY  rrrrrkkrrrrrrrrR   R     ",
    "    YY  rrRrrrrrrrrrrrrrr    R    ",
    "   YY  rrRrrrrrrrrrrrrrrrrR   R   ",
    "   Y   rrrrrrrRRrrrrRRrrrrrr  Rr  ",
    "  Y    rRrrrrRRrr   rRRrrrrr   r  ",
    "  Y  rRRrrrrRyR      kRRrrrr    r ",
    " Y  rrrrrrRRRr       krRRrrrr   r ",
    " Y   rrrrr           kkRRrrr     r",
    "YY    rrr           rkrRrrrk     R",
    "Y      r      rkkrrrkkrrrrrk   y R",
    "Y          rrrrrrrrrrrrrkkkkkk yyR",
    "Y        Rrrrrrrrrrrrkrrrrrrrk yyy",
    "Y      yrrrrrrrrrrrrrkrkkkkkrk yyY",
    "Y    RRrrrrrrrrrrrrkkkrkkrkkrk YyY",
    "Y    RrrrrRRRRrrkrkkkkrkrkrkrk Yrr",
    "Y   RrrrrRRRRrkkkkkkkkrkkRkkrk   r",
    "Y   rrrrRRRRrkkrrrrrrkrkkkkkrk   r",
    "Y  rrrrrRRyr kkrkkrrrrrrrrrrkk  Rr",
    "Y  rrrrRRyR  kkrkkrkrrrrrrrkk   Rr",
    "YY rrrrRRyR  kkrkykkrrrkkkrk    rr",
    "YY  rrrRRyr  kkrrrrrrrrkkrrk   Rr ",
    " y  rrrRRyr  kkrrkkrrrrkrkrk   rr ",
    " yR rrrRRRrR kkrkkkrrrrkkkrk  Rrr ",
    "  R rrrRRRrr kkrrrrrrkkrrrkk Rrrr ",
    "  RR  rrRRrry kkrrrkkkkkkkk  rrr  ",
    "   RR krrRrrrR kkkkk       RRrr   ",
    "    Rr rrrrrrr kkrkk      Rrrrr   ",
    "     rr rrrrrrRrkkkk    RRrrrR    ",
    "      rr rrrrrrrRrrrRRRRrrrRy     ",
    "       rr krrrrrrrrrrrrrrryY      ",
    "        rrr krrrrrrrrrrrRR        ",
    "          rrrrrrkkrrrrrR          ",
    "             rrrrrrrr             ",
    "                                  ",
];

/// How many columns [`FAVICON_NARROW`] draws.
pub const FAVICON_NARROW_COLUMNS: usize = 34;

/// Purpose: The named colour one favicon pixel asks for, or none when it is the background.
/// Serves: `cell`, and through it `favicon_lines`.
/// Design: `docs/design/surfaces.md` §3; named colours only, never RGB.
/// Up: `cell` matches it.
/// Down: `ratatui`'s `Color`.
/// Verify: `cargo test -p loong-tui-app`.
/// The palette the picture is written in.
fn ink(pixel: char) -> Option<Color> {
    match pixel {
        'k' => Some(Color::Black),
        'r' => Some(Color::Red),
        'R' => Some(Color::LightRed),
        'y' => Some(Color::Yellow),
        'Y' => Some(Color::LightYellow),
        _ => None,
    }
}

/// Purpose: One screen cell of two pixels: the upper as the glyph's colour, the lower as its background.
/// Contract: called by `favicon_lines`; a pixel with no ink leaves that half of the cell alone.
fn cell(top: char, bottom: char) -> Span<'static> {
    match (ink(top), ink(bottom)) {
        (None, None) => Span::raw(" "),
        (Some(top), None) => Span::styled("▀", Style::default().fg(top)),
        (None, Some(bottom)) => Span::styled("▄", Style::default().fg(bottom)),
        (Some(top), Some(bottom)) => Span::styled("▀", Style::default().fg(top).bg(bottom)),
    }
}

/// Purpose: Turn the pixel grid into half-block rows, two pixel rows to one screen row.
/// Serves: `App::draw_splash`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `App::draw_splash`.
/// Down: `cell`.
/// Verify: `cargo test -p loong-tui-app`.
/// The picture as the screen draws it. A short last row is treated as empty rather than
/// panicking: the art is data, and a typo in a 52-line table should render something
/// slightly wrong, not take the screen down.
pub fn favicon_lines(art: &[&str], columns: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(art.len() / 2);
    for pair in art.chunks(2) {
        let top = pair[0];
        let bottom = pair.get(1).copied().unwrap_or("");
        let mut spans = Vec::with_capacity(columns);
        for column in 0..columns {
            spans.push(cell(
                top.chars().nth(column).unwrap_or(' '),
                bottom.chars().nth(column).unwrap_or(' '),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Purpose: Put a short line in the middle of the screen width, so the picture looks drawn rather than left-aligned.
/// Serves: `App::draw_splash`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `App::draw_splash`.
/// Down: no call; it returns a padded `Line`.
/// Verify: `cargo test -p loong-tui-app`.
/// Centred by measured width, not by character count: the inscription's runes and the
/// dragon's half blocks are both one cell wide, but the rule is measured so a future
/// ornament that is not stays centred.
pub fn centered(line: Line<'static>, width: u16) -> Line<'static> {
    let used = line.width();
    let pad = (width as usize).saturating_sub(used) / 2;
    if pad == 0 {
        return line;
    }
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(line.spans);
    Line::from(spans)
}

/// The character 龍 as a 16×16 bitmap, one character per pixel, drawn beside the name.
///
/// Rasterised from Noto Sans CJK at sixteen pixels — the smallest size where every stroke
/// of a sixteen-stroke character still lands on its own pixel. Smaller, and the strokes
/// merge into a red block; larger, and the mark stops fitting in a header.
pub const LONG: [&str; 16] = [
    "....#....#......",
    "....#....#......",
    ".#######.######.",
    "..#...#..#......",
    "..#..#...######.",
    ".#######.....##.",
    ".........######.",
    "..#####..#####..",
    ".##...#..#####..",
    ".######..#......",
    ".##...#..#####..",
    ".##...#..#......",
    ".######..######.",
    ".##...#..#....#.",
    ".##...#..#....#.",
    ".##..##..######.",
];

/// How many columns [`LONG`] draws.
pub const LONG_COLUMNS: usize = 16;

/// The character's own colour: ANSI red, the favicon's red and the one hue that reads both
/// on the owner's light background and on a dark one. Not black, which the character used
/// to be knocked out in, and not the frame's magenta, which would make it furniture.
const GLYPH_INK: Color = Color::Red;

/// Purpose: Draw the character as half blocks, two pixel rows to one screen row.
/// Serves: `App::draw_banner`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `App::draw_banner`.
/// Down: `GLYPH_INK`.
/// Verify: `cargo test -p loong-tui-app`.
/// A set pixel over an unset one is `▀`, the reverse `▄`, both `█`: the terminal draws the
/// upper pixel as the glyph's colour and the lower as the background, so one cell carries
/// two rows of the bitmap and the character stays square instead of being stretched.
pub fn glyph_lines(art: &[&str], columns: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(art.len() / 2);
    for pair in art.chunks(2) {
        let top = pair[0];
        let bottom = pair.get(1).copied().unwrap_or("");
        let mut spans = Vec::with_capacity(columns);
        for column in 0..columns {
            let above = is_set(top, column);
            let below = is_set(bottom, column);
            spans.push(match (above, below) {
                (false, false) => Span::raw(" "),
                (true, false) => Span::styled("▀", Style::default().fg(GLYPH_INK)),
                (false, true) => Span::styled("▄", Style::default().fg(GLYPH_INK)),
                (true, true) => Span::styled("█", Style::default().fg(GLYPH_INK)),
            });
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// Purpose: Whether the bitmap sets the pixel at this column.
/// Contract: called by `glyph_lines`; a short row is treated as unset rather than panicking.
fn is_set(row: &str, column: usize) -> bool {
    row.as_bytes().get(column) == Some(&b'#')
}
