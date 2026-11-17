//! Typed view of a validated pack (format v1). Every text is NFC-normalised;
//! nothing else is changed (design §5.2 "All text").

use std::fmt;

/// `format` of every pack.
pub const FORMAT: &str = "vocab-pack";
/// Format versions this crate can validate (design §5.6).
pub const KNOWN_FORMAT_VERSIONS: &[u32] = &[1];

macro_rules! text_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            /// All values in their JSON spelling.
            pub const ALL: &'static [&'static str] = &[$($text),+];

            /// The JSON (and database) spelling.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            /// Parse the JSON (and database) spelling.
            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(
    /// `source.type`.
    SourceType { Book = "book", Film = "film", Series = "series", Article = "article", General = "general" }
);
text_enum!(
    /// Word `goal` (R-15).
    Goal { Recognise = "recognise", Active = "active" }
);
text_enum!(
    /// Sentence `origin` (R-20).
    Origin { Book = "book", Source = "source", Example = "example" }
);
text_enum!(
    /// Word `tag.kind`.
    TagKind { LowPriority = "low-priority", SeenBefore = "seen-before", NotFromSource = "not-from-source" }
);

/// A validation finding with the JSON pointer of the offending value
/// (`/words/3/sentences/1/en`) and a message for the owner. Messages never
/// quote sentence text (R-24: reports are stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub pointer: String,
    pub code: IssueCode,
    pub message: String,
}

/// Machine-readable kind of an [`Issue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IssueCode {
    /// A property the format does not define; ignored (warning).
    UnknownProperty,
    /// A required property is missing.
    Missing,
    /// Wrong JSON type, value out of range, bad pattern, too long, …
    Invalid,
    /// `format_version` this app does not know.
    UnknownVersion,
    /// Control characters other than newline and tab.
    ControlCharacter,
    /// The `[[…]]` rule of a sentence.
    Marker,
    /// Same headword (by key) earlier in the pack.
    DuplicateWord,
    /// Same sentence (by key) earlier in the word; dropped (warning).
    DuplicateSentence,
    /// Fewer than three valid sentences (warning).
    FewSentences,
    /// Any other warning.
    Advice,
}

impl IssueCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownProperty => "unknown_property",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::UnknownVersion => "unknown_version",
            Self::ControlCharacter => "control_character",
            Self::Marker => "marker",
            Self::DuplicateWord => "duplicate_word",
            Self::DuplicateSentence => "duplicate_sentence",
            Self::FewSentences => "few_sentences",
            Self::Advice => "advice",
        }
    }
}

/// `source` of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSource {
    /// Stable slug, e.g. `test-novel-2`.
    pub id: String,
    pub kind: SourceType,
    /// Optional: an existing source keeps its title; a new one without a
    /// title gets a default (decided by the catalog).
    pub title: Option<String>,
    pub author: Option<String>,
    pub series: Option<String>,
    pub series_number: Option<u32>,
    pub url: Option<String>,
}

/// `part` of a multi-pack export (1 of 1 when absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub index: u32,
    pub total: u32,
}

/// Pack-level fields of a pack that passed the pack-level rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackHeader {
    pub format_version: u32,
    pub pack_id: String,
    /// The `YYYY-MM-DD` part of `pack_id`.
    pub pack_date: String,
    /// The `n` at the end of `pack_id`.
    pub pack_number: u32,
    /// `YYYY-MM-DD`.
    pub exported_at: Option<String>,
    pub part: Part,
    pub source: PackSource,
    pub reading_position: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackForm {
    pub form: String,
    pub common: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackTag {
    pub kind: TagKind,
    pub text: Option<String>,
}

/// The word's `tag`: leaving it out says nothing (an existing tag stays);
/// `null` explicitly removes the tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagField {
    Absent,
    Null,
    Tag(PackTag),
}

impl TagField {
    /// The tag, if one is set.
    pub fn tag(&self) -> Option<&PackTag> {
        match self {
            Self::Tag(tag) => Some(tag),
            Self::Absent | Self::Null => None,
        }
    }
}

/// A sentence that passed the sentence rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSentence {
    /// English text with its `[[…]]` span.
    pub en: String,
    /// Czech text with its `[[…]]` span.
    pub cs: String,
    pub origin: Origin,
    pub location: Option<String>,
    /// [`crate::keys::sentence_key`] of `en`.
    pub key: String,
}

/// A word that passed the word rules. Optional scalars are `None` when the
/// pack leaves them out, sets them to `null`, or (text) leaves them blank:
/// the merge treats all three as "nothing to say" (design §5.4 rule 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackWord {
    /// Position in the pack's `words` array (0-based).
    pub index: usize,
    pub headword: String,
    /// [`crate::keys::headword_key`] of `headword`.
    pub key: String,
    pub grammar: Option<String>,
    pub forms: Vec<PackForm>,
    pub pronunciation: Option<String>,
    pub speak: Option<String>,
    pub meaning: String,
    pub czech: Vec<String>,
    pub usefulness: u8,
    pub goal: Option<Goal>,
    pub accepted: Vec<String>,
    pub confusables: Vec<String>,
    pub hint: Option<String>,
    pub spelling: Option<bool>,
    pub spelling_note: Option<String>,
    pub note: Option<String>,
    pub tag: TagField,
    pub icon: Option<String>,
    /// Explicit order within the source (P-1); `None` = derive (§5.5).
    pub seq: Option<u32>,
    /// Valid sentences in pack order, duplicates (by key) removed.
    pub sentences: Vec<PackSentence>,
    /// Sentences that were dropped (sentence-scoped errors).
    pub sentence_errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

/// A word dropped because of word-scoped errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidWord {
    pub index: usize,
    /// The headword if it could be read.
    pub headword: Option<String>,
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordResult {
    /// Boxed: a valid word is much larger than an invalid one.
    Valid(Box<PackWord>),
    Invalid(InvalidWord),
}

/// Result of [`crate::validate::validate`] for one pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackValidation {
    /// The pack header, or the pack-level errors that reject the pack.
    pub pack: Result<PackHeader, Vec<Issue>>,
    /// Per-word results (also filled when the pack is rejected, so every
    /// problem is listed at once).
    pub words: Vec<WordResult>,
    /// Pack-level warnings.
    pub warnings: Vec<Issue>,
}

impl PackValidation {
    /// True when the importer would accept every part of the pack: no pack,
    /// word or sentence errors (warnings allowed).
    pub fn is_clean(&self) -> bool {
        self.pack.is_ok()
            && self.words.iter().all(|w| match w {
                WordResult::Valid(word) => word.sentence_errors.is_empty(),
                WordResult::Invalid(_) => false,
            })
    }

    /// Every warning of the pack, its words and their sentences.
    pub fn all_warnings(&self) -> impl Iterator<Item = &Issue> {
        self.warnings
            .iter()
            .chain(self.words.iter().flat_map(|w| match w {
                WordResult::Valid(word) => word.warnings.iter(),
                WordResult::Invalid(word) => word.warnings.iter(),
            }))
    }
}
