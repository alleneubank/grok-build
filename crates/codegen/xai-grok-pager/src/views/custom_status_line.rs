//! Command-backed custom status line rendered under the prompt.
//!
//! Compatible with Claude Code `statusLine` and Codex `tui.custom_status_line`:
//! the host feeds a JSON snapshot on stdin; the command prints one ANSI line.
//! Named / indexed / truecolor segments are remapped through Grok's active
//! [`Theme`] so the row feels native next to the prompt chrome.

use ansi_to_tui::IntoText;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::theme::Theme;

/// Max padding rows above the rendered line (matches Codex).
pub const MAX_PADDING: u16 = 2;

/// Rows reserved when a line is present: `padding + 1`.
pub fn height(has_line: bool, padding: u16) -> u16 {
    if has_line {
        padding.min(MAX_PADDING).saturating_add(1)
    } else {
        0
    }
}

/// First stdout line that is non-empty after ANSI strip.
pub fn first_renderable_line(output: &str) -> Option<&str> {
    output.lines().map(str::trim_end).find(|line| {
        let plain = strip_ansi_escapes::strip_str(line);
        !plain.trim().is_empty()
    })
}

/// Parse command stdout into the first renderable ANSI line (raw, still
/// colored as the command emitted it). Theming is applied at paint time.
pub fn raw_line_from_command_output(output: &str) -> Option<String> {
    first_renderable_line(output).map(|s| s.to_string())
}

/// Parse a raw ANSI status line and remap its colors into `theme`.
pub fn themed_line(ansi: &str, theme: &Theme) -> Line<'static> {
    let text = match ansi.as_bytes().into_text() {
        Ok(t) => t,
        Err(_) => {
            let plain = strip_ansi_escapes::strip_str(ansi);
            return Line::from(Span::styled(
                plain.trim_end().to_string(),
                Style::default().fg(theme.text_primary),
            ));
        }
    };
    let Some(line) = text.lines.into_iter().next() else {
        return Line::from("");
    };
    Line::from(
        line.spans
            .into_iter()
            .map(|s| {
                let content = s.content.to_string();
                let fg = remap_fg(s.style.fg, theme);
                let bg = remap_bg(s.style.bg, theme);
                let mut style = Style::default().fg(fg);
                if let Some(bg) = bg {
                    style = style.bg(bg);
                }
                // Preserve bold/dim/etc. from the renderer when present.
                style = style
                    .add_modifier(s.style.add_modifier)
                    .remove_modifier(s.style.sub_modifier);
                Span::styled(content, style)
            })
            .collect::<Vec<_>>(),
    )
}

/// Paint the custom status line into `area` (padding rows then the themed line).
pub fn render(area: Rect, buf: &mut Buffer, ansi: &str, padding: u16, theme: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let pad = padding.min(MAX_PADDING).min(area.height.saturating_sub(1));
    let line_y = area.y.saturating_add(pad);
    if line_y >= area.y.saturating_add(area.height) {
        return;
    }
    let line_rect = Rect {
        x: area.x,
        y: line_y,
        width: area.width,
        height: 1,
    };
    themed_line(ansi, theme).render(line_rect, buf);
}

fn remap_fg(c: Option<Color>, theme: &Theme) -> Color {
    match c {
        None | Some(Color::Reset) => theme.text_primary,
        Some(Color::Green) | Some(Color::LightGreen) => theme.accent_success,
        Some(Color::Cyan) | Some(Color::LightCyan) => theme.running,
        Some(Color::Yellow) | Some(Color::LightYellow) => theme.warning,
        Some(Color::Red) | Some(Color::LightRed) => theme.accent_error,
        Some(Color::Blue) | Some(Color::LightBlue) => theme.accent_system,
        Some(Color::Magenta) | Some(Color::LightMagenta) => theme.accent_assistant,
        Some(Color::DarkGray) | Some(Color::Black) => theme.gray_dim,
        Some(Color::Gray) => theme.gray,
        Some(Color::White) => theme.text_primary,
        Some(Color::Indexed(i)) => remap_indexed_fg(i, theme),
        Some(Color::Rgb(r, g, b)) => remap_rgb_fg(r, g, b, theme),
    }
}

fn remap_bg(c: Option<Color>, theme: &Theme) -> Option<Color> {
    match c {
        // Inherit Grok chrome background — no foreign black/reset patches.
        None | Some(Color::Reset) => None,
        Some(Color::DarkGray) | Some(Color::Black) | Some(Color::Gray) => Some(theme.bg_highlight),
        Some(Color::Indexed(i)) => remap_indexed_bg(i, theme),
        Some(Color::Rgb(r, g, b)) => remap_rgb_bg(r, g, b, theme),
        // Named bright bgs are rare on statuslines; fold into highlight.
        Some(_) => Some(theme.bg_highlight),
    }
}

fn remap_indexed_fg(i: u8, theme: &Theme) -> Color {
    // xterm grayscale ramp 232–255 and common dim grays (244–250).
    if (232..=255).contains(&i) {
        let t = (i - 232) as f32 / 23.0;
        return if t < 0.33 {
            theme.gray_dim
        } else if t < 0.66 {
            theme.gray
        } else {
            theme.gray_bright
        };
    }
    match i {
        0 | 8 => theme.gray_dim,                         // black / bright black
        1 | 9 => theme.accent_error,                     // red
        2 | 10 => theme.accent_success,                  // green
        3 | 11 => theme.warning,                         // yellow
        4 | 12 => theme.accent_system,                   // blue
        5 | 13 => theme.accent_assistant,                // magenta
        6 | 14 => theme.running,                         // cyan
        7 | 15 => theme.text_primary,                    // white
        240..=245 => theme.gray_dim,                     // common dim labels ("ask")
        246..=250 => theme.gray,
        251..=255 => theme.gray_bright,
        _ => theme.gray,
    }
}

fn remap_indexed_bg(i: u8, theme: &Theme) -> Option<Color> {
    if (232..=245).contains(&i) || matches!(i, 0 | 8 | 236..=239) {
        Some(theme.bg_highlight)
    } else if (246..=255).contains(&i) {
        Some(theme.bg_visual)
    } else {
        Some(theme.bg_highlight)
    }
}

/// Map truecolor fg (e.g. gauge fill) onto the closest theme semantic color.
fn remap_rgb_fg(r: u8, g: u8, b: u8, theme: &Theme) -> Color {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    // Near-gray: dim hierarchy by luminance.
    if max.saturating_sub(min) < 24 {
        let luma = (r as u16 + g as u16 + b as u16) / 3;
        return if luma < 80 {
            theme.gray_dim
        } else if luma < 160 {
            theme.gray
        } else {
            theme.gray_bright
        };
    }
    // Hue by dominant channel (statusline gauges are typically pure-ish greens).
    if g >= r && g >= b {
        if g > 180 && r < 100 {
            theme.accent_success // bright green fill
        } else if r > 100 {
            theme.warning // yellow-green
        } else {
            theme.accent_success
        }
    } else if r >= g && r >= b {
        if g > r / 2 {
            theme.warning
        } else {
            theme.accent_error
        }
    } else {
        // blue / cyan dominant
        if g > b / 2 {
            theme.running
        } else {
            theme.accent_system
        }
    }
}

fn remap_rgb_bg(r: u8, g: u8, b: u8, theme: &Theme) -> Option<Color> {
    let luma = (r as u16 + g as u16 + b as u16) / 3;
    if luma < 100 {
        Some(theme.bg_highlight)
    } else {
        Some(theme.bg_visual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> Theme {
        Theme::current()
    }

    #[test]
    fn height_zero_without_line() {
        assert_eq!(height(false, 0), 0);
        assert_eq!(height(false, 2), 0);
    }

    #[test]
    fn height_includes_padding() {
        assert_eq!(height(true, 0), 1);
        assert_eq!(height(true, 1), 2);
        assert_eq!(height(true, 5), 3); // capped at MAX_PADDING+1
    }

    #[test]
    fn first_renderable_skips_blank_and_ansi_only() {
        assert_eq!(first_renderable_line("\n\n  \n"), None);
        assert_eq!(
            first_renderable_line("\x1b[32mhello\x1b[0m\nworld"),
            Some("\x1b[32mhello\x1b[0m")
        );
        assert_eq!(first_renderable_line("\nworld"), Some("world"));
    }

    #[test]
    fn raw_line_preserves_ansi() {
        let raw = raw_line_from_command_output("\x1b[36m~/proj\x1b[0m\n").expect("line");
        assert!(raw.contains("\x1b[36m"));
        assert!(raw.contains("~/proj"));
    }

    #[test]
    fn themed_line_maps_named_ansi_to_theme() {
        let theme = test_theme();
        let line = themed_line("\x1b[36mpath\x1b[0m \x1b[32mbranch\x1b[0m", &theme);
        assert!(line.spans.len() >= 2);
        // Cyan → running, green → accent_success
        assert_eq!(line.spans[0].style.fg, Some(theme.running));
        // Find the green span
        let green = line
            .spans
            .iter()
            .find(|s| s.content.contains("branch"))
            .expect("branch span");
        assert_eq!(green.style.fg, Some(theme.accent_success));
    }

    #[test]
    fn themed_line_reset_bg_inherits() {
        let theme = test_theme();
        // After reset, bg should be None (inherit Grok chrome), not Color::Reset.
        let line = themed_line("\x1b[36mhi\x1b[0mthere", &theme);
        for span in &line.spans {
            assert_ne!(
                span.style.bg,
                Some(Color::Reset),
                "Reset bg must not leak into the buffer"
            );
        }
    }

    #[test]
    fn themed_line_truecolor_gauge_uses_theme() {
        let theme = test_theme();
        // Same shape agent-statusline emits for the context gauge.
        let ansi = "\x1b[48;2;60;60;60m\x1b[38;2;30;255;0m▎    \x1b[0m";
        let line = themed_line(ansi, &theme);
        let gauge = line
            .spans
            .iter()
            .find(|s| s.content.contains('▎'))
            .expect("gauge span");
        assert_eq!(gauge.style.fg, Some(theme.accent_success));
        assert_eq!(gauge.style.bg, Some(theme.bg_highlight));
    }
}
