//! Numbers as English words ("1990" -> "nineteen ninety"), so that the
//! lexicon can pronounce them. Only the forms that occur in running text:
//! integers (with thousands separators), decimals, years, decades ("1990s")
//! and ordinals ("21st").

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const SCALES: [(u64, &str); 3] = [
    (1_000_000_000, "billion"),
    (1_000_000, "million"),
    (1_000, "thousand"),
];
/// Longer digit strings (phone numbers, ids) are read digit by digit.
const MAX_CARDINAL_DIGITS: usize = 12;

/// The words of a number token, or `None` when `token` (lowercase) is not a
/// number form this module reads.
pub fn words(token: &str) -> Option<Vec<String>> {
    let digits_end = token
        .find(|c: char| !(c.is_ascii_digit() || c == ',' || c == '.'))
        .unwrap_or(token.len());
    let (body, suffix) = token.split_at(digits_end);
    if !body.starts_with(|c: char| c.is_ascii_digit())
        || !body.ends_with(|c: char| c.is_ascii_digit())
    {
        return None;
    }
    let (integer, fraction) = match body.split_once('.') {
        Some((i, f)) if !f.contains(['.', ',']) => (i, Some(f)),
        Some(_) => return None,
        None => (body, None),
    };
    let plain = !integer.contains(',');
    let integer = grouped_digits(integer)?;
    let mut out = match (fraction, suffix) {
        (Some(fraction), "") => {
            let mut out = cardinal_or_digits(&integer);
            out.push("point".to_owned());
            out.extend(fraction.chars().map(digit_word));
            return Some(out);
        }
        (Some(_), _) => return None,
        (None, "" | "s") if plain && integer.len() == 4 => year(&integer),
        (None, "" | "s" | "st" | "nd" | "rd" | "th") => cardinal_or_digits(&integer),
        (None, _) => return None,
    };
    match suffix {
        "s" => {
            let last = out.pop()?;
            out.push(plural(&last));
        }
        "st" | "nd" | "rd" | "th" => {
            let last = out.pop()?;
            out.push(ordinal(&last));
        }
        _ => {}
    }
    Some(out)
}

/// `1,234,567` -> `1234567`; a comma must separate groups of three digits.
fn grouped_digits(integer: &str) -> Option<String> {
    let mut groups = integer.split(',');
    let first = groups.next()?;
    if first.is_empty() || (integer.contains(',') && first.len() > 3) {
        return None;
    }
    let mut out = first.to_owned();
    for group in groups {
        if group.len() != 3 {
            return None;
        }
        out.push_str(group);
    }
    out.bytes().all(|b| b.is_ascii_digit()).then_some(out)
}

fn digit_word(c: char) -> String {
    let index = c.to_digit(10).unwrap_or(0) as usize;
    ONES[index].to_owned()
}

fn cardinal_or_digits(digits: &str) -> Vec<String> {
    let trimmed = digits.trim_start_matches('0');
    if digits.len() > MAX_CARDINAL_DIGITS || (digits.len() > 1 && digits.starts_with('0')) {
        // "007", a long id: digit by digit.
        return digits.chars().map(digit_word).collect();
    }
    match trimmed.parse::<u64>() {
        Ok(n) => cardinal(n),
        Err(_) => vec![ONES[0].to_owned()], // all zeros: "0"
    }
}

/// A four-digit number read as a year where English does: 1100–1999 and
/// 2010–2099 in pairs ("nineteen oh five", "twenty twenty"), others whole.
fn year(digits: &str) -> Vec<String> {
    let Ok(n) = digits.parse::<u64>() else {
        return cardinal_or_digits(digits);
    };
    if !((1100..=1999).contains(&n) || (2010..=2099).contains(&n)) {
        return cardinal_or_digits(digits);
    }
    let (high, low) = (n / 100, n % 100);
    let mut out = below_hundred(high);
    match low {
        0 => out.push("hundred".to_owned()),
        1..=9 => {
            out.push("oh".to_owned());
            out.extend(below_hundred(low));
        }
        _ => out.extend(below_hundred(low)),
    }
    out
}

fn cardinal(n: u64) -> Vec<String> {
    if n == 0 {
        return vec![ONES[0].to_owned()];
    }
    let mut out = Vec::new();
    let mut rest = n;
    for (scale, name) in SCALES {
        if rest >= scale {
            out.extend(below_thousand(rest / scale));
            out.push(name.to_owned());
            rest %= scale;
        }
    }
    if rest > 0 {
        out.extend(below_thousand(rest));
    }
    out
}

fn below_thousand(n: u64) -> Vec<String> {
    let mut out = Vec::new();
    if n >= 100 {
        out.extend(below_hundred(n / 100));
        out.push("hundred".to_owned());
    }
    if !n.is_multiple_of(100) {
        out.extend(below_hundred(n % 100));
    }
    out
}

fn below_hundred(n: u64) -> Vec<String> {
    let n = (n % 100) as usize;
    if n < 20 {
        return vec![ONES[n].to_owned()];
    }
    let mut out = vec![TENS[n / 10].to_owned()];
    if !n.is_multiple_of(10) {
        out.push(ONES[n % 10].to_owned());
    }
    out
}

fn ordinal(word: &str) -> String {
    match word {
        "one" => "first".to_owned(),
        "two" => "second".to_owned(),
        "three" => "third".to_owned(),
        "five" => "fifth".to_owned(),
        "eight" => "eighth".to_owned(),
        "nine" => "ninth".to_owned(),
        "twelve" => "twelfth".to_owned(),
        _ => match word.strip_suffix('y') {
            Some(stem) => format!("{stem}ieth"),
            None => format!("{word}th"),
        },
    }
}

fn plural(word: &str) -> String {
    match word.strip_suffix('y') {
        Some(stem) => format!("{stem}ies"),
        None => format!("{word}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(token: &str) -> Option<String> {
        words(token).map(|w| w.join(" "))
    }

    #[test]
    fn r40_cardinals() {
        assert_eq!(said("0").as_deref(), Some("zero"));
        assert_eq!(said("7").as_deref(), Some("seven"));
        assert_eq!(said("42").as_deref(), Some("forty two"));
        assert_eq!(said("115").as_deref(), Some("one hundred fifteen"));
        assert_eq!(said("12,000").as_deref(), Some("twelve thousand"));
        assert_eq!(said("3,000,001").as_deref(), Some("three million one"));
        assert_eq!(said("007").as_deref(), Some("zero zero seven"));
    }

    #[test]
    fn r40_years_and_decades() {
        assert_eq!(said("1990").as_deref(), Some("nineteen ninety"));
        assert_eq!(said("1905").as_deref(), Some("nineteen oh five"));
        assert_eq!(said("1800").as_deref(), Some("eighteen hundred"));
        assert_eq!(said("2004").as_deref(), Some("two thousand four"));
        assert_eq!(said("2024").as_deref(), Some("twenty twenty four"));
        assert_eq!(said("1960s").as_deref(), Some("nineteen sixties"));
        assert_eq!(
            said("1,990").as_deref(),
            Some("one thousand nine hundred ninety")
        );
    }

    #[test]
    fn r40_ordinals_and_decimals() {
        assert_eq!(said("1st").as_deref(), Some("first"));
        assert_eq!(said("3rd").as_deref(), Some("third"));
        assert_eq!(said("21st").as_deref(), Some("twenty first"));
        assert_eq!(said("40th").as_deref(), Some("fortieth"));
        assert_eq!(said("12th").as_deref(), Some("twelfth"));
        assert_eq!(said("3.14").as_deref(), Some("three point one four"));
    }

    #[test]
    fn r40_not_numbers() {
        for token in ["abc", "b12", "1.2.3", "1,23", "12px", ".5", "5."] {
            assert_eq!(said(token), None, "{token}");
        }
    }
}
