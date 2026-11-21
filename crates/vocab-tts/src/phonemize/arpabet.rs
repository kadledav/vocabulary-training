//! CMUdict (ARPAbet) pronunciations in Kokoro's misaki notation, per
//! accent: the fallback dictionary for words the Kokoro lexicons lack.
//!
//! The mapping follows the conventions of the misaki lexicons shipped with
//! the model: stress marks (`ˈ` primary, `ˌ` secondary) right before the
//! stressed vowel, diphthongs as single capitals (`A` eɪ, `I` aɪ, `O`/`Q`
//! oʊ/əʊ, `W` aʊ, `Y` ɔɪ). British English is non-rhotic: an `R` that no
//! vowel follows is dropped and the vowel before it becomes long or a
//! centring diphthong. CMUdict does not tell the LOT vowel from the PALM
//! vowel (both `AA`); for British English `AA` is `ɒ`, before `R` `ɑː` —
//! right for most words, an approximation for the rest (counted as such by
//! the caller, which treats every fallback pronunciation as approximate).

use crate::speech::Accent;

/// One CMUdict line (`word PH PH1 …`) as (lowercase word, misaki phonemes),
/// or `None` for comments, alternative pronunciations (`word(2)`) and lines
/// with an unknown phone.
pub fn entry(line: &str, accent: Accent) -> Option<(String, String)> {
    let line = line.split('#').next()?.trim();
    let mut fields = line.split_whitespace();
    let word = fields.next()?;
    if word.contains('(') || word.starts_with(';') {
        return None;
    }
    let phones: Vec<&str> = fields.collect();
    if phones.is_empty() {
        return None;
    }
    Some((word.to_lowercase(), convert(&phones, accent)?))
}

/// ARPAbet phones (with stress digits on vowels) to misaki phonemes.
pub fn convert(phones: &[&str], accent: Accent) -> Option<String> {
    let mut out = String::new();
    for (i, phone) in phones.iter().enumerate() {
        let (base, stress) = match phone.as_bytes().last() {
            Some(d @ b'0'..=b'2') => (&phone[..phone.len() - 1], Some(d - b'0')),
            _ => (*phone, None),
        };
        let next_is_vowel = phones
            .get(i + 1)
            .is_some_and(|p| p.ends_with(['0', '1', '2']));
        let r_follows = phones.get(i + 1) == Some(&"R")
            && !phones
                .get(i + 2)
                .is_some_and(|p| p.ends_with(['0', '1', '2']));
        if base == "R" {
            let after_vowel = i > 0 && phones[i - 1].ends_with(['0', '1', '2']);
            match accent {
                // Non-rhotic: the vowel before took the R (see below).
                Accent::Gb if after_vowel && !next_is_vowel => continue,
                Accent::Us if after_vowel && !next_is_vowel && phones[i - 1].starts_with("ER") => {
                    continue;
                }
                _ => {}
            }
            out.push('ɹ');
            continue;
        }
        if let Some(stress) = stress {
            match stress {
                1 => out.push('ˈ'),
                2 => out.push('ˌ'),
                _ => {}
            }
            out.push_str(vowel(base, stress, r_follows, accent)?);
        } else {
            out.push_str(consonant(base)?);
        }
    }
    Some(out)
}

fn consonant(phone: &str) -> Option<&'static str> {
    Some(match phone {
        "B" => "b",
        "CH" => "ʧ",
        "D" => "d",
        "DH" => "ð",
        "F" => "f",
        "G" => "ɡ",
        "HH" => "h",
        "JH" => "ʤ",
        "K" => "k",
        "L" => "l",
        "M" => "m",
        "N" => "n",
        "NG" => "ŋ",
        "P" => "p",
        "S" => "s",
        "SH" => "ʃ",
        "T" => "t",
        "TH" => "θ",
        "V" => "v",
        "W" => "w",
        "Y" => "j",
        "Z" => "z",
        "ZH" => "ʒ",
        _ => return None,
    })
}

/// A vowel; `r_follows`: an R that closes the syllable comes next.
fn vowel(phone: &str, stress: u8, r_follows: bool, accent: Accent) -> Option<&'static str> {
    let stressed = stress > 0;
    Some(match accent {
        Accent::Us => match phone {
            "AA" => "ɑ",
            "AE" => "æ",
            "AH" if stressed => "ʌ",
            "AH" => "ə",
            "AO" => "ɔ",
            "AW" => "W",
            "AY" => "I",
            "EH" => "ɛ",
            "ER" if stressed => "ɜɹ",
            "ER" => "əɹ",
            "EY" => "A",
            "IH" => "ɪ",
            "IY" => "i",
            "OW" => "O",
            "OY" => "Y",
            "UH" => "ʊ",
            "UW" => "u",
            _ => return None,
        },
        Accent::Gb => match phone {
            "AA" if r_follows => "ɑː",
            "AA" => "ɒ",
            "AE" => "a",
            "AH" if stressed => "ʌ",
            "AH" => "ə",
            "AO" => "ɔː",
            "AW" if r_follows => "Wə",
            "AW" => "W",
            "AY" if r_follows => "Iə",
            "AY" => "I",
            "EH" if r_follows => "ɛː",
            "EH" => "ɛ",
            "ER" if stressed => "ɜː",
            "ER" => "ə",
            "EY" => "A",
            "IH" if r_follows => "ɪə",
            "IH" => "ɪ",
            "IY" if stressed => "iː",
            "IY" => "i",
            "OW" => "Q",
            "OY" => "Y",
            "UH" | "UW" if r_follows => "ʊə",
            "UH" => "ʊ",
            "UW" if stressed => "uː",
            "UW" => "u",
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both(line: &str) -> (String, String) {
        let us = entry(line, Accent::Us).expect("test setup").1;
        let gb = entry(line, Accent::Gb).expect("test setup").1;
        (us, gb)
    }

    #[test]
    fn r42_cmudict_entries_match_the_kokoro_lexicon_conventions() {
        // Expected values as in lexicon-us-en.txt / lexicon-gb-en.txt.
        assert_eq!(both("cat K AE1 T"), ("kˈæt".into(), "kˈat".into()));
        assert_eq!(both("car K AA1 R"), ("kˈɑɹ".into(), "kˈɑː".into()));
        assert_eq!(both("nurse N ER1 S"), ("nˈɜɹs".into(), "nˈɜːs".into()));
        assert_eq!(both("lot L AA1 T"), ("lˈɑt".into(), "lˈɒt".into()));
        assert_eq!(both("about AH0 B AW1 T"), ("əbˈWt".into(), "əbˈWt".into()));
        assert_eq!(both("see S IY1"), ("sˈi".into(), "sˈiː".into()));
        assert_eq!(both("air EH1 R"), ("ˈɛɹ".into(), "ˈɛː".into()));
        assert_eq!(
            both("letter L EH1 T ER0"),
            ("lˈɛtəɹ".into(), "lˈɛtə".into())
        );
        assert_eq!(both("go G OW1"), ("ɡˈO".into(), "ɡˈQ".into()));
        assert_eq!(both("thought TH AO1 T"), ("θˈɔt".into(), "θˈɔːt".into()));
        // An R before a vowel stays in both accents.
        assert_eq!(both("very V EH1 R IY0"), ("vˈɛɹi".into(), "vˈɛɹi".into()));
    }

    #[test]
    fn r40_comments_variants_and_unknown_phones_are_skipped() {
        assert_eq!(entry("# a comment", Accent::Us), None);
        assert_eq!(entry("read(2) R EH1 D", Accent::Us), None);
        assert_eq!(entry("zzz QQ1", Accent::Us), None);
        assert_eq!(
            entry("Kafka K AA1 F K AH0 # name", Accent::Us),
            Some(("kafka".into(), "kˈɑfkə".into()))
        );
    }
}
