//! Finding an emoji by what it is called.
//!
//! A query is matched against every emoji's name and keywords, in English
//! and in the search language, and against its shortcodes. Case and
//! accents do not count ("corazon" finds "corazón"). What matches at the
//! start of a name comes before what matches a word of it, and both before
//! what only contains the query. `:fire:` and `:thumbs` look at shortcodes
//! first, and a few emoticons (`:)`, `<3`) stand for their emoji.

use super::data::{EmojiSet, LangPack};
use std::sync::Arc;

/// The text without case and without accents, as the matching sees it.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars().flat_map(char::to_lowercase) {
        match ch {
            // Combining accents, Arabic vowel marks, variation selectors.
            '\u{0300}'..='\u{036F}' | '\u{064B}'..='\u{0652}' | '\u{FE0F}' => {}
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
            'ď' | 'đ' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
            'ĥ' | 'ħ' => out.push('h'),
            'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
            'ĵ' => out.push('j'),
            'ķ' => out.push('k'),
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => out.push('l'),
            'ñ' | 'ń' | 'ņ' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
            'ŕ' | 'ŗ' | 'ř' => out.push('r'),
            'ś' | 'ŝ' | 'ş' | 'š' => out.push('s'),
            'ţ' | 'ť' | 'ŧ' => out.push('t'),
            'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
            'ŵ' => out.push('w'),
            'ý' | 'ÿ' | 'ŷ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ё' => out.push('е'),
            'й' => out.push('и'),
            // Curly quotes and the like read as their plain form.
            '’' | '‘' => out.push('\''),
            other => out.push(other),
        }
    }
    out
}

/// The emoji an emoticon stands for.
pub fn emoticon(text: &str) -> Option<&'static str> {
    Some(match text {
        ":)" | ":-)" | "(:" => "🙂",
        ":D" | ":-D" => "😃",
        "xD" | "XD" => "😆",
        ";)" | ";-)" => "😉",
        ":(" | ":-(" => "🙁",
        ":'(" | ":,(" => "😢",
        ":P" | ":-P" | ":p" | ":-p" => "😛",
        ";P" | ";p" => "😜",
        ":O" | ":-O" | ":o" | ":-o" => "😮",
        ":*" | ":-*" => "😘",
        ":|" | ":-|" => "😐",
        ":/" | ":-/" | ":\\" => "😕",
        ":$" => "😳",
        "B)" | "B-)" | "8)" => "😎",
        ">:(" => "😠",
        "D:" => "😧",
        "<3" => "❤️",
        "</3" => "💔",
        ":3" => "😺",
        "O:)" | "0:)" => "😇",
        _ => return None,
    })
}

/// What a string of an emoji is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Its name, in English or in the search language.
    Name,
    /// A keyword.
    Keyword,
    /// A shortcode.
    Code,
}

/// The emoji people use most, the most used first: among emoji that match
/// a query equally well, these come first ("heart" in any language is the
/// red one before the one with an arrow).
const POPULAR: [&str; 40] = [
    "😂", "❤️", "🤣", "👍", "😭", "🙏", "😘", "🥰", "😍", "😊", "🎉", "😁", "💕", "🥺", "😅", "🔥",
    "☺️", "🤦", "♥️", "🤷", "🙄", "😆", "🤗", "😉", "🎂", "🤔", "👏", "🙂", "😳", "🥳", "😎", "👌",
    "💜", "😔", "💪", "✨", "💖", "👀", "😋", "😏",
];

/// One emoji, as the matching sees it.
#[derive(Debug)]
struct Entry {
    texts: Vec<(Kind, String)>,
    /// Its place among [`POPULAR`], or their number.
    popularity: usize,
}

/// Every emoji's names, keywords and shortcodes, folded once.
#[derive(Debug)]
pub struct Index {
    set: Arc<EmojiSet>,
    entries: Vec<Entry>,
}

/// How well something matched: the rank of the match, then how much longer
/// than the query the matched string is. Lower is better.
type Match = (u32, usize);

/// Whether `text` or one of its words starts with `token`. Words are split
/// at anything that is neither a letter nor a digit.
fn word_starts(text: &str, token: &str) -> bool {
    text.split(|ch: char| !ch.is_alphanumeric())
        .any(|word| !word.is_empty() && word.starts_with(token))
}

/// How well `token` matches one string; lower is better.
fn score(kind: Kind, text: &str, token: &str) -> Option<u32> {
    let rank = |exact, start, word, inside| {
        if text == token {
            Some(exact)
        } else if text.starts_with(token) {
            Some(start)
        } else if word_starts(text, token) {
            Some(word)
        } else if text.contains(token) {
            Some(inside)
        } else {
            None
        }
    };
    match kind {
        Kind::Name => rank(0, 1, 2, 6),
        Kind::Code => rank(0, 3, 4, 7),
        Kind::Keyword => rank(3, 4, 4, 7),
    }
}

impl Entry {
    /// The best match of `token` among the entry's strings of the wanted
    /// kinds.
    fn best(&self, token: &str, wanted: impl Fn(Kind) -> bool) -> Option<Match> {
        self.texts
            .iter()
            .filter(|(kind, _)| wanted(*kind))
            .filter_map(|(kind, text)| {
                Some((
                    score(*kind, text, token)?,
                    text.len().saturating_sub(token.len()),
                ))
            })
            .min()
    }
}

impl Index {
    /// Folds the names of the set, and of a language's pack beside them.
    pub fn new(set: Arc<EmojiSet>, pack: Option<&LangPack>) -> Self {
        let entries = set
            .emojis
            .iter()
            .enumerate()
            .map(|(index, emoji)| {
                let mut texts = vec![(Kind::Name, fold(&emoji.name))];
                texts.extend(
                    emoji
                        .keywords
                        .iter()
                        .map(|word| (Kind::Keyword, fold(word))),
                );
                texts.extend(
                    emoji
                        .shortcodes
                        .iter()
                        .map(|code| (Kind::Code, code.to_lowercase())),
                );
                if let Some(local) = pack.and_then(|pack| pack.get(index)) {
                    for name in [&local.name, &local.regional] {
                        if !name.is_empty() {
                            texts.push((Kind::Name, fold(name)));
                        }
                    }
                    texts.extend(
                        local
                            .keywords
                            .iter()
                            .map(|word| (Kind::Keyword, fold(word))),
                    );
                }
                let popularity = POPULAR
                    .iter()
                    .position(|popular| super::data::key(popular) == super::data::key(&emoji.text))
                    .unwrap_or(POPULAR.len());
                Entry { texts, popularity }
            })
            .collect();
        Self { set, entries }
    }

    /// The emoji matching `query`, the best first, at most `limit`: places
    /// among the set's emoji. Empty for an empty query.
    pub fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let mut found = Vec::new();
        // An emoticon, or an emoji itself (pasted, or typed with the
        // system's own keyboard), is what it says.
        if let Some(at) = emoticon(query).and_then(|emoji| self.set.find(emoji)) {
            found.push(at);
        }
        if let Some(at) = self.set.find(query) {
            return vec![at];
        }

        let shortcode = query.starts_with(':');
        let folded = fold(query.trim_matches(':'));
        let folded = folded.trim();
        if folded.is_empty() {
            return found;
        }
        let tokens: Vec<&str> = folded
            .split(|ch: char| ch.is_whitespace() || (shortcode && ch == '_'))
            .filter(|token| !token.is_empty())
            .collect();

        // Rank, then how much it is used, then how close in length the
        // match is, then the keyboard's own order.
        let mut scored: Vec<(u32, usize, usize, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                // Between colons the shortcodes are what is meant: a match
                // there comes before any other.
                if shortcode {
                    if let Some((score, extra)) = entry.best(folded, |kind| kind == Kind::Code) {
                        return Some((score.min(5), entry.popularity, extra, index));
                    }
                }
                // The whole query as one phrase, then each of its words:
                // every word has to match something.
                let phrase = entry.best(folded, |_| true);
                let words = (tokens.len() > 1 || shortcode)
                    .then(|| {
                        tokens.iter().try_fold((1, 0), |(score, extra), token| {
                            let (more, longer) = entry.best(token, |_| true)?;
                            Some((score + more, extra + longer))
                        })
                    })
                    .flatten();
                let (score, extra) = match (phrase, words) {
                    (Some(phrase), Some(words)) => phrase.min(words),
                    (phrase, words) => phrase.or(words)?,
                };
                let score = if shortcode { score + 8 } else { score };
                Some((score, entry.popularity, extra, index))
            })
            .collect();
        scored.sort_unstable();
        for (_, _, _, index) in scored {
            if !found.contains(&index) {
                found.push(index);
            }
            if found.len() >= limit {
                break;
            }
        }
        found.truncate(limit);
        found
    }

    /// The shortcode being typed at the end of `text` (what stands before
    /// the caret): a colon at the start or after a space, then at least
    /// two letters, digits, `_`, `+` or `-`, and nothing else. Returned
    /// without the colon.
    pub fn typing(text: &str) -> Option<&str> {
        let colon = text.rfind(':')?;
        let typed = &text[colon + 1..];
        let before = text[..colon].chars().next_back();
        let starts = before.is_none_or(|ch| ch.is_whitespace());
        let word = typed.chars().count() >= 2
            && typed
                .chars()
                .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '+' | '-'));
        (starts && word).then_some(typed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emoji::data::Lang;

    fn index(lang: Lang) -> Index {
        Index::new(EmojiSet::load(), LangPack::load(lang).as_deref())
    }

    /// The emoji found for a query, as text.
    fn found(index: &Index, query: &str) -> Vec<String> {
        index
            .search(query, 50)
            .into_iter()
            .map(|at| index.set.emojis[at].text.to_string())
            .collect()
    }

    fn first(index: &Index, query: &str) -> String {
        found(index, query)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("nothing for {query:?}"))
    }

    #[test]
    fn case_and_accents_do_not_count() {
        assert_eq!(fold("Corazón"), "corazon");
        assert_eq!(fold("CAFÉ"), "cafe");
        assert_eq!(fold("niño"), "nino");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("ÇA VA"), "ca va");
        assert_eq!(fold("coração"), "coracao");
        assert_eq!(fold("İstanbul"), "istanbul");
        assert_eq!(fold("ёлка"), "елка");
        // Written with a combining accent, it is the same word.
        assert_eq!(fold("corazo\u{301}n"), "corazon");
        // Scripts without case or accents are left alone.
        assert_eq!(fold("आग"), "आग");
    }

    #[test]
    fn english_is_ranked_by_where_the_query_matches() {
        let index = index(Lang::En);
        assert_eq!(first(&index, "fire"), "🔥");
        assert_eq!(first(&index, "FIRE"), "🔥");
        assert_eq!(first(&index, "thumbs up"), "👍");
        assert_eq!(first(&index, "pizza"), "🍕");

        // "heart": the emoji whose shortcode it is, then names that start
        // with it, then names with a word that does, before keywords.
        let hearts = found(&index, "heart");
        assert_eq!(hearts[0], "❤️");
        let place = |emoji: &str| {
            hearts
                .iter()
                .position(|found| found == emoji)
                .unwrap_or_else(|| panic!("{emoji} not found"))
        };
        assert!(place("💘") < place("💙"), "starts with < has the word");
        assert!(place("💙") < place("💌"), "a name < a keyword");

        // A prefix finds it, and so does the middle of a word, later.
        assert!(found(&index, "fir").contains(&"🔥".to_owned()));
        let inside = found(&index, "ire");
        assert!(inside.contains(&"🔥".to_owned()));
        assert_ne!(inside[0], "🔥", "a start of a word comes first");
        // Every word has to match.
        assert_eq!(first(&index, "red heart"), "❤️");
        assert!(found(&index, "heart zzzz").is_empty());
        assert!(found(&index, "zzzzqq").is_empty());
        assert!(found(&index, "   ").is_empty());
    }

    #[test]
    fn spanish_is_searched_beside_english_with_or_without_accents() {
        let index = index(Lang::Es);
        assert_eq!(first(&index, "fuego"), "🔥");
        assert_eq!(first(&index, "corazón"), "❤️");
        assert_eq!(first(&index, "corazon"), "❤️");
        assert_eq!(first(&index, "CORAZÓN"), "❤️");
        assert!(found(&index, "risa").contains(&"😂".to_owned()));
        assert!(found(&index, "pulgar").contains(&"👍".to_owned()));
        // The way Latin America says it finds it too.
        assert!(found(&index, "cierre").contains(&"🤐".to_owned()));
        assert!(found(&index, "cremallera").contains(&"🤐".to_owned()));
        // English still works.
        assert_eq!(first(&index, "fire"), "🔥");
        assert_eq!(first(&index, "heart"), "❤️");

        // An English index does not know the Spanish words.
        let english = self::index(Lang::En);
        assert!(!found(&english, "fuego").contains(&"🔥".to_owned()));
    }

    #[test]
    fn other_languages_find_their_words() {
        assert_eq!(first(&index(Lang::Pt), "fogo"), "🔥");
        assert_eq!(first(&index(Lang::Pt), "coracao"), "❤️");
        assert_eq!(first(&index(Lang::Fr), "feu"), "🔥");
        assert_eq!(first(&index(Lang::De), "feuer"), "🔥");
        assert_eq!(first(&index(Lang::It), "fuoco"), "🔥");
        assert_eq!(first(&index(Lang::Hi), "आग"), "🔥");
    }

    #[test]
    fn shortcodes_and_emoticons_are_understood() {
        let index = index(Lang::En);
        assert_eq!(first(&index, ":fire:"), "🔥");
        assert_eq!(first(&index, ":fire"), "🔥");
        assert_eq!(first(&index, ":thumbsup:"), "👍");
        assert_eq!(first(&index, ":+1:"), "👍");
        assert_eq!(first(&index, ":fir"), "🔥");
        // Between colons a shortcode comes before a name.
        assert_eq!(first(&index, ":heart"), "❤️");
        // Words joined the shortcode way find names too.
        assert!(found(&index, ":red_heart").contains(&"❤️".to_owned()));

        assert_eq!(first(&index, ":)"), "🙂");
        assert_eq!(first(&index, "<3"), "❤️");
        assert_eq!(first(&index, ":D"), "😃");
        assert_eq!(emoticon("nope"), None);

        // An emoji finds itself, in any skin tone.
        assert_eq!(found(&index, "🔥"), ["🔥"]);
        assert_eq!(found(&index, "👍🏽"), ["👍"]);
    }

    #[test]
    fn a_shortcode_being_typed_is_told_from_other_colons() {
        assert_eq!(Index::typing(":fir"), Some("fir"));
        assert_eq!(Index::typing("on my way :thumbs"), Some("thumbs"));
        assert_eq!(Index::typing("see you\n:wa"), Some("wa"));
        assert_eq!(Index::typing(":+1"), Some("+1"));
        // One letter is too little to guess from, and a time is no code.
        assert_eq!(Index::typing(":f"), None);
        assert_eq!(Index::typing("at 10:30"), None);
        assert_eq!(Index::typing("https://wuapi"), None);
        assert_eq!(Index::typing("note:this"), None);
        // Finished, or left behind by a space.
        assert_eq!(Index::typing(":fire: "), None);
        assert_eq!(Index::typing(":fire now"), None);
        assert_eq!(Index::typing("no colon"), None);
        assert_eq!(Index::typing(""), None);
    }
}
