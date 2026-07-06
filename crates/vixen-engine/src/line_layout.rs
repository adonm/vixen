//! Minimal inline line-layout slice for Phase 4.
//!
//! This is not the full Servo `layout_2020` integration. It is the first
//! executable layout state behind `vixen_engine::page::Page`: collapse body
//! text, wrap it into deterministic line boxes for a viewport, and feed
//! `vixen-headless --dump-lines`. The full layout adapter will replace the
//! glyph-width estimate with real font/text metrics while keeping this facade
//! shape.

#![forbid(unsafe_code)]

/// A single laid-out text line in physical viewport pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct LineBox {
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub text: String,
}

/// Deterministic inputs for the initial text line builder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineLayoutConfig {
    pub viewport_width: u32,
    pub margin_px: f32,
    pub line_height_px: f32,
    pub average_char_width_px: f32,
}

impl LineLayoutConfig {
    pub fn for_viewport(viewport: (u32, u32)) -> Self {
        Self {
            viewport_width: viewport.0,
            margin_px: 8.0,
            line_height_px: 19.2,
            average_char_width_px: 8.0,
        }
    }

    fn max_chars_per_line(self) -> usize {
        let available = (self.viewport_width as f32 - self.margin_px * 2.0).max(1.0);
        (available / self.average_char_width_px.max(1.0))
            .floor()
            .max(1.0) as usize
    }
}

/// Build deterministic line boxes from visible text.
pub fn layout_text_lines(text: &str, config: LineLayoutConfig) -> Vec<LineBox> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }

    let max_chars = config.max_chars_per_line();
    let mut raw_lines = Vec::new();
    let mut current = String::new();

    for word in words {
        if word.chars().count() > max_chars {
            if !current.is_empty() {
                raw_lines.push(std::mem::take(&mut current));
            }
            push_hard_wrapped_word(word, max_chars, &mut raw_lines);
            continue;
        }

        let candidate_len = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > max_chars && !current.is_empty() {
            raw_lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }

    if !current.is_empty() {
        raw_lines.push(current);
    }

    raw_lines
        .into_iter()
        .enumerate()
        .map(|(idx, text)| LineBox {
            index: idx + 1,
            x: config.margin_px,
            y: config.margin_px + idx as f32 * config.line_height_px,
            w: text.chars().count() as f32 * config.average_char_width_px,
            h: config.line_height_px,
            text,
        })
        .collect()
}

/// Render the stable `--dump-lines` text format.
pub fn dump_line_boxes(lines: &[LineBox], viewport: (u32, u32)) -> String {
    let mut out = format!(
        "# line-boxes viewport={}x{} count={}\n",
        viewport.0,
        viewport.1,
        lines.len()
    );
    for line in lines {
        out.push_str(&format!(
            "line {}: x={:.1} y={:.1} w={:.1} h={:.1} text=\"{}\"\n",
            line.index,
            line.x,
            line.y,
            line.w,
            line.h,
            escape_dump_text(&line.text)
        ));
    }
    out
}

fn push_hard_wrapped_word(word: &str, max_chars: usize, out: &mut Vec<String>) {
    let mut chunk = String::new();
    for ch in word.chars() {
        if chunk.chars().count() == max_chars {
            out.push(std::mem::take(&mut chunk));
        }
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
}

fn escape_dump_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(width: u32) -> LineLayoutConfig {
        LineLayoutConfig {
            viewport_width: width,
            margin_px: 0.0,
            line_height_px: 10.0,
            average_char_width_px: 1.0,
        }
    }

    #[test]
    fn wraps_text_on_word_boundaries() {
        let lines = layout_text_lines("one two three four", cfg(9));
        let text: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, vec!["one two", "three", "four"]);
        assert_eq!(lines[1].y, 10.0);
    }

    #[test]
    fn hard_wraps_long_words() {
        let lines = layout_text_lines("abcdefg hi", cfg(3));
        let text: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, vec!["abc", "def", "g", "hi"]);
    }

    #[test]
    fn empty_or_whitespace_text_yields_no_lines() {
        assert!(layout_text_lines(" \n\t ", cfg(10)).is_empty());
    }

    #[test]
    fn dump_format_is_stable_and_escaped() {
        let lines = layout_text_lines("a \"quote\"", cfg(20));
        let dump = dump_line_boxes(&lines, (20, 10));
        assert!(dump.contains("# line-boxes viewport=20x10 count=1"));
        assert!(dump.contains("line 1: x=0.0 y=0.0 w=9.0 h=10.0 text=\"a \\\"quote\\\"\""));
    }
}
