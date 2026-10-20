//! Word-wrapping page text into display lines, keeping track of where each link lands
//! so the UI can underline it and draw a box around the selected one.
//!
//! Widths come from the blitstr2 glyph tables, measured the way the GAM typesetter lays
//! text out, so the positions we compute match what ends up on screen.

use blitstr2::GlyphStyle;

/// The reader's three text sizes.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FontSize {
    Small,
    Regular,
    Large,
}

impl FontSize {
    pub fn style(self) -> GlyphStyle {
        match self {
            FontSize::Small => GlyphStyle::Small,
            FontSize::Regular => GlyphStyle::Regular,
            FontSize::Large => GlyphStyle::Large,
        }
    }

    /// Height of the glyph box (see `blitstr2::glyph_to_height_hint`).
    pub fn glyph_h(self) -> isize {
        match self {
            FontSize::Small => 12,
            FontSize::Regular => 15,
            FontSize::Large => 24,
        }
    }

    /// Line pitch: the glyphs plus room for a link underline.
    pub fn row_h(self) -> isize {
        match self {
            FontSize::Small => 15,
            FontSize::Regular => 18,
            FontSize::Large => 28,
        }
    }

    pub fn larger(self) -> FontSize {
        match self {
            FontSize::Small => FontSize::Regular,
            _ => FontSize::Large,
        }
    }

    pub fn smaller(self) -> FontSize {
        match self {
            FontSize::Large => FontSize::Regular,
            _ => FontSize::Small,
        }
    }

    /// The next size round, wrapping from Large back to Small.
    pub fn cycle(self) -> FontSize {
        match self {
            FontSize::Small => FontSize::Regular,
            FontSize::Regular => FontSize::Large,
            FontSize::Large => FontSize::Small,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FontSize::Small => "Small",
            FontSize::Regular => "Regular",
            FontSize::Large => "Large",
        }
    }

    pub fn from_label(s: &str) -> Option<FontSize> {
        [FontSize::Small, FontSize::Regular, FontSize::Large].into_iter().find(|f| f.label() == s)
    }

    /// Large glyphs are drawn doubled and can spill a few pixels past their measured
    /// width, so leave them more room on the right.
    pub fn right_gap(self) -> isize { if self == FontSize::Large { 22 } else { 8 } }
}

/// A stretch of page text, either plain or part of a link (an index into the view's
/// list of links).
pub struct Run<'a> {
    pub text: &'a str,
    pub link: Option<usize>,
}

/// Where a link sits on one display line, in pixels from the line's left edge.
#[derive(Debug, PartialEq)]
pub struct LinkSpan {
    pub link: usize,
    pub x0: isize,
    pub x1: isize,
}

#[derive(Default)]
pub struct TextLine {
    pub text: String,
    pub links: Vec<LinkSpan>,
}

type Tagged = (char, Option<usize>);

/// Wrap `runs` into lines at most `width` pixels wide. Paragraph breaks (`\n`) and
/// each paragraph's indentation are kept; other runs of whitespace become one space.
/// A word wider than a whole line is broken across lines.
pub fn layout(runs: &[Run], width: isize, font: FontSize) -> Vec<TextLine> {
    let chars: Vec<Tagged> =
        runs.iter().flat_map(|run| run.text.chars().map(move |c| (c, run.link))).collect();

    let mut out = Vec::new();
    for paragraph in chars.split(|&(c, _)| c == '\n') {
        layout_paragraph(paragraph, width, font, &mut out);
    }
    while out.last().is_some_and(|line: &TextLine| line.text.is_empty()) {
        out.pop();
    }
    out
}

fn layout_paragraph(paragraph: &[Tagged], width: isize, font: FontSize, out: &mut Vec<TextLine>) {
    let words: Vec<&[Tagged]> =
        paragraph.split(|&(c, _)| c.is_whitespace()).filter(|word| !word.is_empty()).collect();
    if words.is_empty() {
        out.push(TextLine::default());
        return;
    }

    let space = advance(' ', font);
    let indent: isize = paragraph
        .iter()
        .take_while(|&&(c, _)| c.is_whitespace())
        .map(|&(c, _)| if c == '\t' { 4 } else { 1 })
        .sum();
    let indent = indent.min(width / 3 / space.max(1));

    let mut line: Vec<Tagged> = (0..indent).map(|_| (' ', None)).collect();
    let mut line_px = indent * space;
    let mut has_word = false;

    for (i, word) in words.iter().enumerate() {
        let word_px: isize = word.iter().map(|&(c, _)| advance(c, font)).sum();
        if has_word {
            if line_px + space + word_px <= width {
                // The space between two words of the same link is part of the link.
                let before = words[i - 1].last().and_then(|&(_, link)| link);
                let gap_link = if before.is_some() && before == word[0].1 { before } else { None };
                line.push((' ', gap_link));
                line.extend_from_slice(word);
                line_px += space + word_px;
                continue;
            }
            out.push(finish(&line, font));
            line.clear();
            line_px = 0;
        } else if line_px + word_px > width && word_px <= width {
            // No room for the first word after the indentation: drop the indentation
            // rather than break up a word that fits on a line of its own.
            line.clear();
            line_px = 0;
        }
        if line_px + word_px <= width {
            line.extend_from_slice(word);
            line_px += word_px;
        } else {
            for &tagged in word.iter() {
                let adv = advance(tagged.0, font);
                if line_px + adv > width && !line.is_empty() {
                    out.push(finish(&line, font));
                    line.clear();
                    line_px = 0;
                }
                line.push(tagged);
                line_px += adv;
            }
        }
        has_word = true;
    }
    if !line.is_empty() {
        out.push(finish(&line, font));
    }
}

/// Turn a line of tagged characters into its text plus the pixel span of each link on it.
fn finish(line: &[Tagged], font: FontSize) -> TextLine {
    let mut text = String::new();
    let mut links: Vec<LinkSpan> = Vec::new();
    let mut x = 0;
    for &(c, link) in line {
        let adv = advance(c, font);
        if let Some(link) = link {
            match links.last_mut() {
                Some(span) if span.link == link && span.x1 == x => span.x1 = x + adv,
                _ => links.push(LinkSpan { link, x0: x, x1: x + adv }),
            }
        }
        text.push(c);
        x += adv;
    }
    TextLine { text, links }
}

/// Horizontal advance of one glyph in pixels, as the GAM typesetter lays it out: glyph
/// width plus kerning, with spaces unkerned. Characters outside the Latin fonts (emoji,
/// CJK) are given a full-square width. (Same measure as apps/ollama-client.)
pub fn advance(ch: char, font: FontSize) -> isize {
    let glyph = match font {
        FontSize::Small => blitstr2::small_glyph(ch),
        FontSize::Regular => blitstr2::regular_glyph(ch),
        FontSize::Large => blitstr2::large_glyph(ch),
    };
    match glyph {
        Ok(g) if ch == ' ' => g.wide as isize,
        Ok(g) => (g.wide + g.kern) as isize,
        Err(_) => {
            if font == FontSize::Large {
                32
            } else {
                16
            }
        }
    }
}

pub fn text_px(s: &str, font: FontSize) -> isize { s.chars().map(|c| advance(c, font)).sum() }

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[TextLine]) -> Vec<&str> { lines.iter().map(|l| l.text.as_str()).collect() }

    #[test]
    fn wraps_and_keeps_paragraphs() {
        let runs = [Run { text: "one two three\n\n  indented four\n", link: None }];
        let width = text_px("  indented", FontSize::Regular);
        let lines = layout(&runs, width, FontSize::Regular);
        assert_eq!(texts(&lines), vec!["one two", "three", "", "  indented", "four"]);
    }

    #[test]
    fn drops_indentation_that_leaves_no_room() {
        let runs = [Run { text: "    abcdefgh", link: None }];
        let lines = layout(&runs, text_px("abcdefgh", FontSize::Regular), FontSize::Regular);
        assert_eq!(texts(&lines), vec!["abcdefgh"]);
    }

    #[test]
    fn tracks_link_spans_across_wraps() {
        let runs = [
            Run { text: "See ", link: None },
            Run { text: "Meeting Notes", link: Some(0) },
            Run { text: ", then ", link: None },
            Run { text: "Ideas", link: Some(1) },
            Run { text: ".", link: None },
        ];
        let font = FontSize::Regular;
        let lines = layout(&runs, text_px("See Meeting", font), font);
        assert_eq!(texts(&lines), vec!["See Meeting", "Notes, then", "Ideas."]);

        let see = text_px("See ", font);
        assert_eq!(lines[0].links, vec![LinkSpan { link: 0, x0: see, x1: text_px("See Meeting", font) }]);
        assert_eq!(lines[1].links, vec![LinkSpan { link: 0, x0: 0, x1: text_px("Notes", font) }]);
        assert_eq!(lines[2].links, vec![LinkSpan { link: 1, x0: 0, x1: text_px("Ideas", font) }]);
    }

    #[test]
    fn link_words_on_one_line_are_one_span() {
        let runs = [Run { text: "Meeting Notes", link: Some(3) }];
        let lines = layout(&runs, 1000, FontSize::Small);
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(lines[0].links[0].x1, text_px("Meeting Notes", FontSize::Small));
    }

    #[test]
    fn breaks_overlong_words() {
        let runs = [Run { text: "abcdefghij", link: None }];
        let lines = layout(&runs, text_px("abcd", FontSize::Regular), FontSize::Regular);
        assert_eq!(texts(&lines), vec!["abcd", "efgh", "ij"]);
    }
}
