//! Positions in text, as the different parts of mNote count them (ADR-0005):
//!
//! - [`CharOffset`]: characters from the start, as Vim counts;
//! - [`ByteOffset`]: UTF-8 bytes from the start, as tree-sitter counts;
//! - [`Point`]: a row and a byte column, tree-sitter's position;
//! - [`LspPosition`]: a line and a column in the [`PositionEncoding`] agreed
//!   with a language server (UTF-16 by default, UTF-8 or UTF-32 if both
//!   sides support it).
//!
//! They're distinct types, converted only through [`RopeCoords`], so a byte
//! offset can't be used where a character offset is meant. Every conversion
//! clamps instead of panicking: a position past the end of a line or of the
//! text lands at that end, and one inside a character lands at its start.

use ropey::Rope;

/// A position in characters from the start of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CharOffset(pub usize);

/// A position in UTF-8 bytes from the start of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ByteOffset(pub usize);

/// A row (0-based line) and a column in bytes from the line's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Point {
    pub row: usize,
    pub column_bytes: usize,
}

impl From<Point> for tree_sitter::Point {
    fn from(p: Point) -> Self {
        tree_sitter::Point::new(p.row, p.column_bytes)
    }
}

/// How an LSP position counts the characters of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PositionEncoding {
    /// UTF-8 bytes.
    Utf8,
    /// UTF-16 code units: LSP's default.
    #[default]
    Utf16,
    /// Unicode scalar values (characters).
    Utf32,
}

impl PositionEncoding {
    /// Its name in LSP's `positionEncoding` (`"utf-16"`).
    pub fn name(self) -> &'static str {
        match self {
            PositionEncoding::Utf8 => "utf-8",
            PositionEncoding::Utf16 => "utf-16",
            PositionEncoding::Utf32 => "utf-32",
        }
    }

    /// The encoding LSP names `name`, if it's one of the three.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "utf-8" => PositionEncoding::Utf8,
            "utf-16" => PositionEncoding::Utf16,
            "utf-32" => PositionEncoding::Utf32,
            _ => return None,
        })
    }
}

/// A position as LSP sends it: a 0-based line and a column counted in some
/// [`PositionEncoding`]. It means nothing without that encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

/// Where a character offset is, in every unit at once: computed once (for
/// example when an edit is recorded, while the old text still exists) and
/// turned into whichever unit a consumer needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TextPoint {
    pub row: usize,
    pub column_bytes: usize,
    pub column_utf16: usize,
    pub column_chars: usize,
}

impl TextPoint {
    /// Its tree-sitter point.
    pub fn point(&self) -> Point {
        Point {
            row: self.row,
            column_bytes: self.column_bytes,
        }
    }

    /// Its LSP position in `encoding`.
    pub fn lsp(&self, encoding: PositionEncoding) -> LspPosition {
        let column = match encoding {
            PositionEncoding::Utf8 => self.column_bytes,
            PositionEncoding::Utf16 => self.column_utf16,
            PositionEncoding::Utf32 => self.column_chars,
        };
        LspPosition {
            line: to_u32(self.row),
            character: to_u32(column),
        }
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Conversions between the position types, on a rope.
pub trait RopeCoords {
    fn char_to_byte_offset(&self, at: CharOffset) -> ByteOffset;
    fn byte_to_char_offset(&self, at: ByteOffset) -> CharOffset;
    fn char_to_point(&self, at: CharOffset) -> Point;
    fn point_to_char(&self, at: Point) -> CharOffset;
    fn text_point(&self, at: CharOffset) -> TextPoint;
    fn char_to_lsp(&self, at: CharOffset, encoding: PositionEncoding) -> LspPosition;
    fn lsp_to_char(&self, at: LspPosition, encoding: PositionEncoding) -> CharOffset;
}

impl RopeCoords for Rope {
    fn char_to_byte_offset(&self, at: CharOffset) -> ByteOffset {
        ByteOffset(self.char_to_byte(at.0.min(self.len_chars())))
    }

    fn byte_to_char_offset(&self, at: ByteOffset) -> CharOffset {
        // `byte_to_char` lands on the start of the character holding the byte.
        CharOffset(self.byte_to_char(at.0.min(self.len_bytes())))
    }

    fn char_to_point(&self, at: CharOffset) -> Point {
        self.text_point(at).point()
    }

    fn point_to_char(&self, at: Point) -> CharOffset {
        let Some((start, len)) = self.line_span(at.row) else {
            return CharOffset(self.len_chars());
        };
        let start_byte = self.char_to_byte(start);
        let end_byte = self.char_to_byte(start + len);
        let byte = (start_byte + at.column_bytes).min(end_byte);
        CharOffset(self.byte_to_char(byte))
    }

    fn text_point(&self, at: CharOffset) -> TextPoint {
        let at = at.0.min(self.len_chars());
        let row = self.char_to_line(at);
        let start = self.line_to_char(row);
        TextPoint {
            row,
            column_bytes: self.char_to_byte(at) - self.char_to_byte(start),
            column_utf16: self.char_to_utf16_cu(at) - self.char_to_utf16_cu(start),
            column_chars: at - start,
        }
    }

    fn char_to_lsp(&self, at: CharOffset, encoding: PositionEncoding) -> LspPosition {
        self.text_point(at).lsp(encoding)
    }

    fn lsp_to_char(&self, at: LspPosition, encoding: PositionEncoding) -> CharOffset {
        let Some((start, len)) = self.line_span(at.line as usize) else {
            return CharOffset(self.len_chars());
        };
        let column = at.character as usize;
        let within = match encoding {
            PositionEncoding::Utf32 => column.min(len),
            PositionEncoding::Utf8 => {
                let start_byte = self.char_to_byte(start);
                let end_byte = self.char_to_byte(start + len);
                self.byte_to_char((start_byte + column).min(end_byte)) - start
            }
            PositionEncoding::Utf16 => {
                let start_cu = self.char_to_utf16_cu(start);
                let end_cu = self.char_to_utf16_cu(start + len);
                // Inside a surrogate pair, `utf16_cu_to_char` lands on the
                // character's start.
                self.utf16_cu_to_char((start_cu + column).min(end_cu)) - start
            }
        };
        CharOffset(start + within)
    }
}

trait LineSpan {
    /// The first character of line `row` and its length without the line
    /// break; `None` past the last line.
    fn line_span(&self, row: usize) -> Option<(usize, usize)>;
}

impl LineSpan for Rope {
    fn line_span(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.len_lines() {
            return None;
        }
        let line = self.line(row);
        let mut len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            len -= 1;
            if len > 0 && line.char(len - 1) == '\r' {
                len -= 1;
            }
        }
        Some((self.line_to_char(row), len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator, so the property tests need no
    /// dependency and fail the same way every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// One, two, three and four UTF-8 bytes; one and two UTF-16 units.
    const CHARS: &[char] = &['a', 'é', '日', '🙂', '𝄞', ' ', '\n'];

    fn text(rng: &mut Rng) -> String {
        (0..rng.below(60))
            .map(|_| CHARS[rng.below(CHARS.len())])
            .collect()
    }

    const ENCODINGS: [PositionEncoding; 3] = [
        PositionEncoding::Utf8,
        PositionEncoding::Utf16,
        PositionEncoding::Utf32,
    ];

    #[test]
    fn every_conversion_round_trips() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..400 {
            let rope = Rope::from_str(&text(&mut rng));
            for c in 0..=rope.len_chars() {
                let at = CharOffset(c);
                assert_eq!(rope.byte_to_char_offset(rope.char_to_byte_offset(at)), at);
                assert_eq!(rope.point_to_char(rope.char_to_point(at)), at);
                for encoding in ENCODINGS {
                    let lsp = rope.char_to_lsp(at, encoding);
                    assert_eq!(
                        rope.lsp_to_char(lsp, encoding),
                        at,
                        "{rope:?} {c} {encoding:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn columns_count_in_each_unit() {
        let rope = Rope::from_str("a🙂é\nx");
        // After "a🙂": 1 + 4 bytes, 1 + 2 UTF-16 units, 2 characters.
        let p = rope.text_point(CharOffset(2));
        assert_eq!(
            (p.row, p.column_bytes, p.column_utf16, p.column_chars),
            (0, 5, 3, 2)
        );
        assert_eq!(
            rope.char_to_point(CharOffset(4)),
            Point {
                row: 1,
                column_bytes: 0
            }
        );
        assert_eq!(
            rope.char_to_lsp(CharOffset(3), PositionEncoding::Utf16),
            LspPosition {
                line: 0,
                character: 4
            }
        );
        assert_eq!(
            rope.char_to_lsp(CharOffset(3), PositionEncoding::Utf8),
            LspPosition {
                line: 0,
                character: 7
            }
        );
    }

    #[test]
    fn positions_inside_a_character_or_past_an_end_clamp() {
        let rope = Rope::from_str("a🙂b\nxy\r\nz");
        let utf16 = PositionEncoding::Utf16;
        // Between the two halves of 🙂's surrogate pair: its start.
        assert_eq!(
            rope.lsp_to_char(
                LspPosition {
                    line: 0,
                    character: 2
                },
                utf16
            ),
            CharOffset(1)
        );
        // Inside 🙂's UTF-8 bytes: its start.
        let utf8 = PositionEncoding::Utf8;
        assert_eq!(
            rope.lsp_to_char(
                LspPosition {
                    line: 0,
                    character: 3
                },
                utf8
            ),
            CharOffset(1)
        );
        // Past the line's end: the end, before the line break (and `\r\n`).
        assert_eq!(
            rope.lsp_to_char(
                LspPosition {
                    line: 0,
                    character: 99
                },
                utf16
            ),
            CharOffset(3)
        );
        assert_eq!(
            rope.lsp_to_char(
                LspPosition {
                    line: 1,
                    character: 99
                },
                utf16
            ),
            CharOffset(6)
        );
        // Past the last line: the end of the text (9 characters).
        assert_eq!(
            rope.lsp_to_char(
                LspPosition {
                    line: 9,
                    character: 0
                },
                utf16
            ),
            CharOffset(9)
        );
        assert_eq!(
            rope.point_to_char(Point {
                row: 0,
                column_bytes: 2
            }),
            CharOffset(1)
        );
        assert_eq!(rope.byte_to_char_offset(ByteOffset(99)), CharOffset(9));
        assert_eq!(
            rope.char_to_byte_offset(CharOffset(99)),
            ByteOffset(rope.len_bytes())
        );
    }

    #[test]
    fn encoding_names() {
        for encoding in ENCODINGS {
            assert_eq!(PositionEncoding::from_name(encoding.name()), Some(encoding));
        }
        assert_eq!(PositionEncoding::from_name("utf-7"), None);
        assert_eq!(PositionEncoding::default(), PositionEncoding::Utf16);
    }
}
