//! WhatsApp's text markup, read into blocks and styled spans.
//!
//! `*bold*`, `_italic_`, `~strike~`, `` `mono` ``, a block between triple
//! backticks, `> quote`, `- item` and `1. item`; links and mentions are
//! found on the way. Pure text in, plain data out: the views decide what a
//! span looks like.

use std::ops::Range;

/// How a stretch of text is written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// `*bold*`.
    pub bold: bool,
    /// `_italic_`.
    pub italic: bool,
    /// `~strike~`.
    pub strike: bool,
    /// `` `mono` ``.
    pub mono: bool,
    /// The address a link opens, complete with its scheme.
    pub link: Option<String>,
    /// The name of a mentioned contact.
    pub mention: bool,
    /// The mention is of the account itself.
    pub mention_me: bool,
    /// Which of the message's mentions it is ([`Handle::who`]).
    pub mention_of: Option<usize>,
}

/// A styled stretch of a [`Line`]'s text, in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Where in the text.
    pub range: Range<usize>,
    /// How it is written.
    pub style: Style,
}

/// Text without its markup characters, and where it is styled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    /// What is shown. May hold line breaks.
    pub text: String,
    /// The styled stretches, in order, never overlapping.
    pub spans: Vec<Span>,
}

impl Line {
    /// The stretch `range` of what is shown, written as it was typed: the
    /// markup characters are back around what they style, so pasting it
    /// into WhatsApp gives the same formatting. A mention stays as it
    /// reads (`@Name`), a link as its text.
    pub fn copied(&self, range: Range<usize>) -> String {
        let end = range.end.min(self.text.len());
        let mut out = String::new();
        let mut at = range.start.min(end);
        let piece = |from: usize, to: usize| self.text.get(from..to).unwrap_or("");
        for span in &self.spans {
            let from = span.range.start.max(at);
            let to = span.range.end.min(end);
            if from >= to {
                continue;
            }
            out.push_str(piece(at, from));
            let style = &span.style;
            // Outermost first, and closed in the opposite order.
            let marks: Vec<&str> = [
                (style.mono, "`"),
                (style.bold, "*"),
                (style.italic, "_"),
                (style.strike, "~"),
            ]
            .into_iter()
            .filter(|(on, _)| *on)
            .map(|(_, mark)| mark)
            .collect();
            // Markup does not reach across spaces at its ends.
            let inner = piece(from, to);
            let body = inner.trim();
            if marks.is_empty() || body.is_empty() {
                out.push_str(inner);
            } else {
                let lead = &inner[..inner.len() - inner.trim_start().len()];
                let tail = &inner[inner.trim_end().len()..];
                out.push_str(lead);
                marks.iter().for_each(|mark| out.push_str(mark));
                out.push_str(body);
                marks.iter().rev().for_each(|mark| out.push_str(mark));
                out.push_str(tail);
            }
            at = to;
        }
        out.push_str(piece(at, end));
        out
    }
}

/// One block of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Ordinary lines.
    Paragraph(Line),
    /// `> quoted`.
    Quote(Line),
    /// `- item` or `* item`.
    Bullet(Line),
    /// `1. item`, with its number as written.
    Numbered(String, Line),
    /// A block between triple backticks, shown as typed.
    Code(String),
}

/// A mention to look for: what follows the `@` in the text, and what to
/// show instead. The digits of an id are never shown: a mention without
/// a name reads as [`client_core::UNKNOWN_PERSON`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Handle {
    /// What follows the `@` in the text.
    pub handle: String,
    /// The contact's name, when known.
    pub name: Option<String>,
    /// It is the account itself.
    pub me: bool,
    /// Which of the message's mentions this is: handed back in
    /// [`Style::mention_of`], so a click finds the person.
    pub who: usize,
}

/// Reads a message's text.
pub fn parse(text: &str, mentions: &[Handle]) -> Vec<Block> {
    // Longest handles first: one number may start like another.
    let mut mentions: Vec<&Handle> = mentions
        .iter()
        .filter(|mention| !mention.handle.is_empty())
        .collect();
    mentions.sort_by_key(|mention| std::cmp::Reverse(mention.handle.len()));

    let mut blocks = Vec::new();
    let mut rest = text;
    // Code blocks first: nothing inside one is markup.
    while let Some(open) = rest.find("```") {
        let after = &rest[open + 3..];
        let Some(close) = after.find("```") else {
            break;
        };
        lines(&rest[..open], &mentions, &mut blocks);
        let code = after[..close].trim_matches('\n');
        if !code.is_empty() {
            blocks.push(Block::Code(code.to_owned()));
        }
        rest = &after[close + 3..];
    }
    lines(rest, &mentions, &mut blocks);
    blocks
}

/// The lines of a stretch of text outside a code block.
fn lines(text: &str, mentions: &[&Handle], blocks: &mut Vec<Block>) {
    let text = text.trim_matches('\n');
    if text.is_empty() {
        return;
    }
    let mut paragraph: Option<Line> = None;
    for raw in text.split('\n') {
        let block = if let Some(quoted) = raw
            .strip_prefix("> ")
            .filter(|quoted| !quoted.trim().is_empty())
        {
            Some(Block::Quote(inline(quoted.trim_start(), mentions)))
        } else if let Some(item) = raw
            .strip_prefix("- ")
            .or(raw.strip_prefix("* "))
            .filter(|item| !item.trim().is_empty())
        {
            Some(Block::Bullet(inline(item, mentions)))
        } else {
            numbered(raw).map(|(number, item)| Block::Numbered(number, inline(item, mentions)))
        };
        match block {
            Some(block) => {
                blocks.extend(paragraph.take().map(Block::Paragraph));
                blocks.push(block);
            }
            None => {
                let line = inline(raw, mentions);
                match &mut paragraph {
                    Some(paragraph) => {
                        paragraph.text.push('\n');
                        let offset = paragraph.text.len();
                        paragraph.text.push_str(&line.text);
                        paragraph
                            .spans
                            .extend(line.spans.into_iter().map(|span| Span {
                                range: span.range.start + offset..span.range.end + offset,
                                style: span.style,
                            }));
                    }
                    None => paragraph = Some(line),
                }
            }
        }
    }
    blocks.extend(paragraph.map(Block::Paragraph));
}

/// `12. item` -> (`12.`, `item`).
fn numbered(line: &str) -> Option<(String, &str)> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=3).contains(&digits) {
        return None;
    }
    let item = line[digits..].strip_prefix(". ")?;
    (!item.trim().is_empty()).then(|| (format!("{}.", &line[..digits]), item))
}

#[derive(Default)]
struct Builder {
    line: Line,
}

impl Builder {
    fn push(&mut self, text: &str, style: &Style) {
        if text.is_empty() {
            return;
        }
        let start = self.line.text.len();
        self.line.text.push_str(text);
        let end = self.line.text.len();
        if *style == Style::default() {
            return;
        }
        match self.line.spans.last_mut() {
            Some(last) if last.range.end == start && last.style == *style => last.range.end = end,
            _ => self.line.spans.push(Span {
                range: start..end,
                style: style.clone(),
            }),
        }
    }
}

/// One line's inline markup.
fn inline(text: &str, mentions: &[&Handle]) -> Line {
    let chars: Vec<char> = text.chars().collect();
    let mut builder = Builder::default();
    spans(&chars, &Style::default(), mentions, &mut builder);
    builder.line
}

const SCHEMES: [&str; 3] = ["https://", "http://", "www."];

/// The link that starts at `at`, as (characters taken, address).
fn link_at(chars: &[char], at: usize) -> Option<(usize, String)> {
    let starts_word = at == 0 || chars[at - 1].is_whitespace() || "(<[".contains(chars[at - 1]);
    if !starts_word {
        return None;
    }
    let scheme = SCHEMES.iter().find(|scheme| {
        scheme.chars().enumerate().all(|(offset, c)| {
            chars
                .get(at + offset)
                .is_some_and(|have| have.eq_ignore_ascii_case(&c))
        })
    })?;
    let mut end = at;
    while end < chars.len() && !chars[end].is_whitespace() && !"<>\"".contains(chars[end]) {
        end += 1;
    }
    // Punctuation that ends the sentence, not the address.
    while end > at && ".,;:!?)]'*_~".contains(chars[end - 1]) {
        end -= 1;
    }
    let shown: String = chars[at..end].iter().collect();
    // Something has to follow the scheme, and it has to look like a host.
    let host = shown.get(scheme.len()..).unwrap_or_default();
    if host.is_empty() || (*scheme == "www." && !host.contains('.')) {
        return None;
    }
    let address = if *scheme == "www." {
        format!("https://{shown}")
    } else {
        shown
    };
    Some((end - at, address))
}

/// The mention that starts at `at` (an `@`), as (characters taken, what
/// to show, whether it is the account itself).
fn mention_at<'h>(
    chars: &[char],
    at: usize,
    mentions: &[&'h Handle],
) -> Option<(usize, String, &'h Handle)> {
    if chars[at] != '@' || (at > 0 && chars[at - 1].is_alphanumeric()) {
        return None;
    }
    mentions.iter().find_map(|mention| {
        let handle: Vec<char> = mention.handle.chars().collect();
        let end = at + 1 + handle.len();
        let matches = chars.get(at + 1..end) == Some(handle.as_slice())
            && !chars.get(end).is_some_and(|next| next.is_alphanumeric());
        matches.then(|| {
            let name = mention
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(client_core::UNKNOWN_PERSON);
            let name = if mention.me { client_core::YOU } else { name };
            (1 + handle.len(), format!("@{name}"), *mention)
        })
    })
}

/// Where the stretch opened by the delimiter at `open` closes.
fn closing(chars: &[char], open: usize) -> Option<usize> {
    let delimiter = chars[open];
    let opens = (open == 0 || !chars[open - 1].is_alphanumeric())
        && chars
            .get(open + 1)
            .is_some_and(|next| !next.is_whitespace());
    if !opens {
        return None;
    }
    (open + 2..chars.len()).find(|&at| {
        chars[at] == delimiter
            && !chars[at - 1].is_whitespace()
            && !chars.get(at + 1).is_some_and(|next| next.is_alphanumeric())
    })
}

fn spans(chars: &[char], style: &Style, mentions: &[&Handle], out: &mut Builder) {
    let mut plain = String::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if let Some((taken, address)) = link_at(chars, at) {
            out.push(&std::mem::take(&mut plain), style);
            let shown: String = chars[at..at + taken].iter().collect();
            out.push(
                &shown,
                &Style {
                    link: Some(address),
                    ..style.clone()
                },
            );
            at += taken;
            continue;
        }
        if let Some((taken, shown, mention)) = mention_at(chars, at, mentions) {
            out.push(&std::mem::take(&mut plain), style);
            out.push(
                &shown,
                &Style {
                    mention: true,
                    mention_me: mention.me,
                    mention_of: Some(mention.who),
                    ..style.clone()
                },
            );
            at += taken;
            continue;
        }
        if c == '`' {
            // Mono is taken as typed: nothing inside it is markup.
            let close = (at + 2..chars.len()).find(|&end| chars[end] == '`');
            if let Some(close) = close {
                out.push(&std::mem::take(&mut plain), style);
                let shown: String = chars[at + 1..close].iter().collect();
                out.push(
                    &shown,
                    &Style {
                        mono: true,
                        ..style.clone()
                    },
                );
                at = close + 1;
                continue;
            }
        }
        if matches!(c, '*' | '_' | '~') {
            if let Some(close) = closing(chars, at) {
                out.push(&std::mem::take(&mut plain), style);
                let mut inner = style.clone();
                match c {
                    '*' => inner.bold = true,
                    '_' => inner.italic = true,
                    _ => inner.strike = true,
                }
                spans(&chars[at + 1..close], &inner, mentions, out);
                at = close + 1;
                continue;
            }
        }
        plain.push(c);
        at += 1;
    }
    out.push(&plain, style);
}

/// Whether a character is (part of) an emoji.
fn is_emoji(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1F000..=0x1FAFF
            | 0x2600..=0x27BF
            | 0x2300..=0x23FF
            | 0x2B00..=0x2BFF
            | 0x2190..=0x21FF
            | 0x25AA..=0x25FE
            | 0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x24C2
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
    )
}

/// How many emoji a text is made of, when it is made of nothing else.
/// `None` for an empty text, or one with anything but emoji and spaces.
pub fn emoji_only(text: &str) -> Option<usize> {
    const JOINER: char = '\u{200D}';
    let mut count = 0usize;
    let mut flags = 0usize;
    let mut joined = false;
    for c in text.chars() {
        match c {
            c if c.is_whitespace() => joined = false,
            JOINER => joined = true,
            // Variation selector, keycap, tags: part of the emoji before.
            '\u{FE0F}' | '\u{20E3}' | '\u{E0020}'..='\u{E007F}' => {}
            // Skin tones modify the emoji before them.
            '\u{1F3FB}'..='\u{1F3FF}' => {}
            // A flag is two regional indicators.
            '\u{1F1E6}'..='\u{1F1FF}' => flags += 1,
            c if is_emoji(c) => {
                if !joined {
                    count += 1;
                }
                joined = false;
            }
            _ => return None,
        }
    }
    let count = count + flags.div_ceil(2);
    (count > 0).then_some(count)
}

/// The first link of a text, complete with its scheme.
#[cfg(test)]
fn first_link(text: &str) -> Option<String> {
    parse(text, &[]).into_iter().find_map(|block| match block {
        Block::Paragraph(line)
        | Block::Quote(line)
        | Block::Bullet(line)
        | Block::Numbered(_, line) => line.spans.into_iter().find_map(|span| span.style.link),
        Block::Code(_) => None,
    })
}

/// The host of an address, for showing where a link leads: `example.com`
/// for `https://www.example.com/page`. `None` when it is not http(s).
pub fn host(address: &str) -> Option<String> {
    let rest = address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // Never show credentials, and no port.
    let host = authority.rsplit('@').next()?.split(':').next()?;
    let host = host.strip_prefix("www.").unwrap_or(host).to_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Whether an address may be handed to the system's browser: http(s) only.
pub fn opens_in_browser(address: &str) -> bool {
    host(address).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_is_copied_as_it_was_written() {
        let blocks = parse("plain *bold* and _it_ `code` ~gone~ end", &[]);
        let Block::Paragraph(line) = &blocks[0] else {
            panic!("a paragraph");
        };
        assert_eq!(line.text, "plain bold and it code gone end");
        // All of it: the original, character for character.
        assert_eq!(
            line.copied(0..line.text.len()),
            "plain *bold* and _it_ `code` ~gone~ end"
        );
        // Part of a styled stretch keeps its style.
        assert_eq!(line.copied(3..8), "in *bo*");
        // Nothing styled: as shown.
        assert_eq!(line.copied(0..5), "plain");
        // Past the end is the end.
        assert_eq!(line.copied(28..400), "end");
        // A mention is copied as it reads.
        let ana = Handle {
            handle: "584245550199".into(),
            name: Some("Ana".into()),
            me: false,
            who: 0,
        };
        let blocks = parse("ask @584245550199 first", &[ana]);
        let Block::Paragraph(line) = &blocks[0] else {
            panic!("a paragraph");
        };
        assert_eq!(line.copied(0..line.text.len()), "ask @Ana first");
    }

    fn line(text: &str) -> Line {
        match parse(text, &[]).as_slice() {
            [Block::Paragraph(line)] => line.clone(),
            other => panic!("one paragraph expected, got {other:?}"),
        }
    }

    fn styled(line: &Line) -> Vec<(&str, &Style)> {
        line.spans
            .iter()
            .map(|span| (&line.text[span.range.clone()], &span.style))
            .collect()
    }

    #[test]
    fn inline_markup_is_removed_and_remembered() {
        let read = line("*bold* _italic_ ~gone~ `a*b` plain");
        assert_eq!(read.text, "bold italic gone a*b plain");
        let spans = styled(&read);
        assert_eq!(spans.len(), 4);
        assert!(spans[0].0 == "bold" && spans[0].1.bold);
        assert!(spans[1].0 == "italic" && spans[1].1.italic);
        assert!(spans[2].0 == "gone" && spans[2].1.strike);
        assert!(spans[3].0 == "a*b" && spans[3].1.mono && !spans[3].1.bold);
    }

    #[test]
    fn styles_nest_and_stray_marks_stay_as_typed() {
        let read = line("*very _much_ so*");
        assert_eq!(read.text, "very much so");
        let spans = styled(&read);
        assert!(spans.iter().all(|(_, style)| style.bold));
        assert!(spans[1].0 == "much" && spans[1].1.italic);

        for typed in ["2 * 3 * 4", "snake_case_name", "a*b", "~", "_"] {
            let read = parse(typed, &[]);
            let Block::Paragraph(read) = &read[0] else {
                panic!("{typed}: {read:?}")
            };
            assert_eq!(read.text, typed);
            assert!(read.spans.is_empty(), "{typed}: {:?}", read.spans);
        }
    }

    #[test]
    fn blocks_are_quotes_lists_and_code() {
        let blocks = parse(
            "Plan:\n- first\n- *second*\n1. one\n2. two\n> said\nafter\n```\nlet x = *1*;\n```\nend",
            &[],
        );
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|block| match block {
                Block::Paragraph(_) => "p",
                Block::Quote(_) => "q",
                Block::Bullet(_) => "b",
                Block::Numbered(..) => "n",
                Block::Code(_) => "c",
            })
            .collect();
        assert_eq!(kinds, ["p", "b", "b", "n", "n", "q", "p", "c", "p"]);
        assert!(matches!(&blocks[2], Block::Bullet(line) if line.text == "second"));
        assert!(matches!(&blocks[4], Block::Numbered(number, line)
            if number == "2." && line.text == "two"));
        assert!(matches!(&blocks[7], Block::Code(code) if code == "let x = *1*;"));
    }

    #[test]
    fn lines_of_a_paragraph_stay_together_with_their_spans() {
        let read = line("one\n*two*\n\nthree");
        assert_eq!(read.text, "one\ntwo\n\nthree");
        assert_eq!(
            styled(&read),
            [(
                "two",
                &Style {
                    bold: true,
                    ..Style::default()
                }
            )]
        );
    }

    #[test]
    fn links_are_found_whole_and_keep_their_underscores() {
        let read = line("See https://example.com/a_b_c?x=1, or www.wuapi.dev.");
        assert_eq!(
            read.text,
            "See https://example.com/a_b_c?x=1, or www.wuapi.dev."
        );
        let spans = styled(&read);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, "https://example.com/a_b_c?x=1");
        assert_eq!(
            spans[0].1.link.as_deref(),
            Some("https://example.com/a_b_c?x=1")
        );
        assert_eq!(spans[1].0, "www.wuapi.dev");
        assert_eq!(spans[1].1.link.as_deref(), Some("https://www.wuapi.dev"));
        assert_eq!(
            first_link("no link here, then http://a.example/x"),
            Some("http://a.example/x".into())
        );
        // Not links: no host, or another scheme.
        for typed in [
            "https://",
            "www.",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ] {
            assert_eq!(first_link(typed), None, "{typed}");
        }
    }

    #[test]
    fn hosts_are_shown_without_credentials_or_www() {
        assert_eq!(
            host("https://www.Example.com/page?x#y"),
            Some("example.com".into())
        );
        assert_eq!(
            host("http://user:pw@shop.example:8080/"),
            Some("shop.example".into())
        );
        assert_eq!(host("ftp://example.com"), None);
        assert_eq!(host("https://"), None);
        assert!(opens_in_browser("https://example.com") && !opens_in_browser("geo:1,2"));
    }

    #[test]
    fn mentions_show_a_name_and_never_the_digits_of_an_id() {
        let mentions = [
            Handle {
                handle: "584245550199".into(),
                name: Some("Ana Rojas".into()),
                me: false,
                who: 0,
            },
            Handle {
                handle: "58424".into(),
                name: None,
                me: false,
                who: 1,
            },
            Handle {
                handle: "56955501234".into(),
                name: Some("Victor".into()),
                me: true,
                who: 2,
            },
        ];
        let blocks = parse(
            "hi @584245550199, and @58424! not@58424 nor @584249 but @56955501234",
            &mentions,
        );
        let Block::Paragraph(read) = &blocks[0] else {
            panic!()
        };
        assert_eq!(
            read.text,
            "hi @Ana Rojas, and @someone! not@58424 nor @584249 but @You"
        );
        let spans = styled(read);
        assert_eq!(spans.len(), 3);
        assert!(spans[0].0 == "@Ana Rojas" && spans[0].1.mention && !spans[0].1.mention_me);
        assert!(spans[1].0 == "@someone" && spans[1].1.mention_of == Some(1));
        assert!(spans[2].0 == "@You" && spans[2].1.mention_me);

        // A mention that the text does not carry has no handle: nothing
        // in the text is taken for it.
        let unbound = [Handle::default()];
        let blocks = parse("the code is @123456", &unbound);
        let Block::Paragraph(read) = &blocks[0] else {
            panic!()
        };
        assert_eq!(read.text, "the code is @123456");
        assert!(styled(read).is_empty());
    }

    #[test]
    fn emoji_only_messages_are_counted() {
        assert_eq!(emoji_only("🎉"), Some(1));
        assert_eq!(emoji_only("🎉 ✈️"), Some(2));
        assert_eq!(
            emoji_only("👍🏽"),
            Some(1),
            "a skin tone is not an emoji more"
        );
        assert_eq!(emoji_only("👨‍👩‍👧"), Some(1), "a joined family is one");
        assert_eq!(emoji_only("🇻🇪🇵🇹"), Some(2), "two flags");
        assert_eq!(emoji_only("ok 👍"), None);
        assert_eq!(emoji_only("123"), None);
        assert_eq!(emoji_only("  "), None);
        assert_eq!(emoji_only(""), None);
    }
}
