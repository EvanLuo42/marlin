//! Locale-aware date and time formatting with CLDR date patterns.

use core::fmt;

use super::Locale;
use super::number::{ascii_digits, write_digits};

/// A day of the proleptic Gregorian calendar.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

impl Date {
    /// `None` unless `month` is 1–12 and `day` exists in that month.
    pub const fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        if month < 1 || month > 12 || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(Self { year, month, day })
    }

    pub const fn year(&self) -> i32 {
        self.year
    }

    /// 1–12.
    pub const fn month(&self) -> u8 {
        self.month
    }

    /// 1–31.
    pub const fn day(&self) -> u8 {
        self.day
    }
}

const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// A time of day.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Time {
    hour: u8,
    minute: u8,
    second: u8,
}

impl Time {
    /// `None` unless `hour < 24`, `minute < 60` and `second < 60`.
    pub const fn new(hour: u8, minute: u8, second: u8) -> Option<Self> {
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        Some(Self {
            hour,
            minute,
            second,
        })
    }

    pub const fn hour(&self) -> u8 {
        self.hour
    }

    pub const fn minute(&self) -> u8 {
        self.minute
    }

    pub const fn second(&self) -> u8 {
        self.second
    }
}

/// Which date pattern to use.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum DateStyle {
    /// Numeric: `1/15/24`, `15.01.24`, `2024/01/15`.
    #[default]
    Short,
    /// With the month's name: `January 15, 2024`, `15. Januar 2024`, `2024年1月15日`.
    Long,
}

/// How a locale writes dates and times: CLDR's Gregorian `dateFormats` (short and long),
/// `timeFormats` (short), wide format-context month names and AM/PM markers.
///
/// Hand-encoded after CLDR (`common/main/<locale>.xml`) for the languages of
/// [`NumberFormat::for_locale`](super::NumberFormat::for_locale), with en-GB as the one regional variant; others get CLDR root's
/// ISO-like patterns.
///
/// Patterns use the CLDR (UTS #35) letters `y` / `yy`, `M` / `MM` / `MMMM`, `d` / `dd`,
/// `H` / `HH`, `h` / `hh`, `m` / `mm`, `s` / `ss` and `a`; `'…'` quotes literal text, `''` is
/// a quote. Other characters are literal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DateFormat {
    pub short_date: &'static str,
    pub long_date: &'static str,
    pub time: &'static str,
    /// Month names as used inside dates (the genitive in Slavic languages, `tammikuuta`…).
    pub months: &'static [&'static str; 12],
    /// AM and PM.
    pub day_periods: [&'static str; 2],
    /// The digit zero (Arabic uses Arabic-Indic digits).
    pub zero: char,
}

const AM_PM: [&str; 2] = ["AM", "PM"];

impl DateFormat {
    /// CLDR root: `2024-01-15`, `2024 M01 15`, `14:05`.
    pub const ROOT: Self = Self {
        short_date: "y-MM-dd",
        long_date: "y MMMM d",
        time: "HH:mm",
        months: &[
            "M01", "M02", "M03", "M04", "M05", "M06", "M07", "M08", "M09", "M10", "M11", "M12",
        ],
        day_periods: AM_PM,
        zero: '0',
    };

    pub fn for_locale(locale: &Locale) -> Self {
        let f = |short_date, long_date, time, months| Self {
            short_date,
            long_date,
            time,
            months,
            ..Self::ROOT
        };
        match (locale.language(), locale.region()) {
            ("en", Some("GB")) => f("dd/MM/y", "d MMMM y", "HH:mm", EN),
            // U+202F NARROW NO-BREAK SPACE before AM/PM, as in CLDR since version 42.
            ("en", _) => f("M/d/yy", "MMMM d, y", "h:mm\u{202f}a", EN),
            ("de", _) => f("dd.MM.yy", "d. MMMM y", "HH:mm", DE),
            ("nl", _) => f("dd-MM-y", "d MMMM y", "HH:mm", NL),
            ("sv", _) => f("y-MM-dd", "d MMMM y", "HH:mm", SV),
            ("da", _) => f("dd.MM.y", "d. MMMM y", "HH.mm", DA),
            ("nb" | "no" | "nn", _) => f("dd.MM.y", "d. MMMM y", "HH:mm", NB),
            ("fi", _) => f("d.M.y", "d. MMMM y", "H.mm", FI),
            ("es", _) => f("d/M/yy", "d 'de' MMMM 'de' y", "H:mm", ES),
            ("it", _) => f("dd/MM/yy", "d MMMM y", "HH:mm", IT),
            ("pt", Some("PT")) => f("dd/MM/yy", "d 'de' MMMM 'de' y", "HH:mm", PT),
            ("pt", _) => f("dd/MM/y", "d 'de' MMMM 'de' y", "HH:mm", PT),
            ("fr", _) => f("dd/MM/y", "d MMMM y", "HH:mm", FR),
            ("ru", _) => f("dd.MM.y", "d MMMM y 'г'.", "HH:mm", RU),
            ("uk", _) => f("dd.MM.yy", "d MMMM y 'р'.", "HH:mm", UK),
            ("pl", _) => f("d.MM.y", "d MMMM y", "HH:mm", PL),
            ("cs", _) => f("dd.MM.yy", "d. MMMM y", "H:mm", CS),
            // U+200F RIGHT-TO-LEFT MARK keeps the slashes in order.
            ("ar", _) => Self {
                day_periods: ["ص", "م"],
                zero: '\u{660}',
                ..f("d\u{200f}/M\u{200f}/y", "d MMMM y", "h:mm a", AR)
            },
            ("he", _) => f("d.M.y", "d בMMMM y", "H:mm", HE),
            ("ja", _) => Self {
                day_periods: ["午前", "午後"],
                ..f("y/MM/dd", "y年M月d日", "H:mm", JA_ZH)
            },
            ("ko", _) => Self {
                day_periods: ["오전", "오후"],
                ..f("yy. M. d.", "y년 M월 d일", "a h:mm", KO)
            },
            ("zh", _) => Self {
                day_periods: ["上午", "下午"],
                ..f("y/M/d", "y年M月d日", "HH:mm", JA_ZH)
            },
            ("tr", _) => f("d.MM.y", "d MMMM y", "HH:mm", TR),
            ("hi", _) => Self {
                day_periods: ["am", "pm"],
                ..f("d/M/yy", "d MMMM y", "h:mm a", HI)
            },
            _ => Self::ROOT,
        }
    }

    pub fn format_date(
        &self,
        date: Date,
        style: DateStyle,
        out: &mut (impl fmt::Write + ?Sized),
    ) -> fmt::Result {
        let pattern = match style {
            DateStyle::Short => self.short_date,
            DateStyle::Long => self.long_date,
        };
        self.format_pattern(pattern, Some(date), None, out)
    }

    pub fn format_time(&self, time: Time, out: &mut (impl fmt::Write + ?Sized)) -> fmt::Result {
        self.format_pattern(self.time, None, Some(time), out)
    }

    /// Writes `pattern` (see the type docs). Date fields without a `date`, and time fields
    /// without a `time`, write nothing.
    pub fn format_pattern(
        &self,
        pattern: &str,
        date: Option<Date>,
        time: Option<Time>,
        out: &mut (impl fmt::Write + ?Sized),
    ) -> fmt::Result {
        let mut rest = pattern;
        while let Some(c) = rest.chars().next() {
            if c == '\'' {
                if let Some(after) = rest.strip_prefix("''") {
                    out.write_char('\'')?;
                    rest = after;
                    continue;
                }
                // Literal text up to a lone quote; `''` inside it is a quote too.
                rest = &rest[1..];
                loop {
                    let Some(end) = rest.find('\'') else {
                        // Unterminated: the rest of the pattern is literal.
                        out.write_str(rest)?;
                        rest = "";
                        break;
                    };
                    out.write_str(&rest[..end])?;
                    rest = &rest[end + 1..];
                    match rest.strip_prefix('\'') {
                        Some(after) => {
                            out.write_char('\'')?;
                            rest = after;
                        }
                        None => break,
                    }
                }
                continue;
            }
            if !c.is_ascii_alphabetic() {
                out.write_char(c)?;
                rest = &rest[c.len_utf8()..];
                continue;
            }
            let count = rest.bytes().take_while(|&b| b == c as u8).count();
            rest = &rest[count..];
            let number = |n: u32, width: usize, out: &mut _| self.number(n, width, out);
            match (c, date, time) {
                ('y', Some(d), _) if count == 2 => {
                    number(d.year.rem_euclid(100) as u32, 2, out)?;
                }
                ('y', Some(d), _) => {
                    if d.year < 0 {
                        out.write_char('-')?;
                    }
                    number(d.year.unsigned_abs(), count, out)?;
                }
                ('M', Some(d), _) if count >= 3 => {
                    out.write_str(self.months[d.month as usize - 1])?
                }
                ('M', Some(d), _) => number(d.month.into(), count, out)?,
                ('d', Some(d), _) => number(d.day.into(), count, out)?,
                ('H', _, Some(t)) => number(t.hour.into(), count, out)?,
                ('h', _, Some(t)) => number((u32::from(t.hour) + 11) % 12 + 1, count, out)?,
                ('m', _, Some(t)) => number(t.minute.into(), count, out)?,
                ('s', _, Some(t)) => number(t.second.into(), count, out)?,
                ('a', _, Some(t)) => out.write_str(self.day_periods[usize::from(t.hour >= 12)])?,
                ('y' | 'M' | 'd' | 'H' | 'h' | 'm' | 's' | 'a', _, _) => {}
                _ => {
                    for _ in 0..count {
                        out.write_char(c)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `n` zero-padded to `width` digits, in this locale's digits.
    fn number(&self, n: u32, width: usize, out: &mut (impl fmt::Write + ?Sized)) -> fmt::Result {
        let mut buf = [0u8; 20];
        let digits = ascii_digits(n.into(), &mut buf);
        for _ in digits.len()..width {
            write_digits(self.zero, "0", out)?;
        }
        write_digits(self.zero, digits, out)
    }
}

type Months = &'static [&'static str; 12];

const EN: Months = &[
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DE: Months = &[
    "Januar",
    "Februar",
    "März",
    "April",
    "Mai",
    "Juni",
    "Juli",
    "August",
    "September",
    "Oktober",
    "November",
    "Dezember",
];
const NL: Months = &[
    "januari",
    "februari",
    "maart",
    "april",
    "mei",
    "juni",
    "juli",
    "augustus",
    "september",
    "oktober",
    "november",
    "december",
];
const SV: Months = &[
    "januari",
    "februari",
    "mars",
    "april",
    "maj",
    "juni",
    "juli",
    "augusti",
    "september",
    "oktober",
    "november",
    "december",
];
const DA: Months = &[
    "januar",
    "februar",
    "marts",
    "april",
    "maj",
    "juni",
    "juli",
    "august",
    "september",
    "oktober",
    "november",
    "december",
];
const NB: Months = &[
    "januar",
    "februar",
    "mars",
    "april",
    "mai",
    "juni",
    "juli",
    "august",
    "september",
    "oktober",
    "november",
    "desember",
];
/// Partitive, as in `15. tammikuuta 2024`.
const FI: Months = &[
    "tammikuuta",
    "helmikuuta",
    "maaliskuuta",
    "huhtikuuta",
    "toukokuuta",
    "kesäkuuta",
    "heinäkuuta",
    "elokuuta",
    "syyskuuta",
    "lokakuuta",
    "marraskuuta",
    "joulukuuta",
];
const ES: Months = &[
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
];
const IT: Months = &[
    "gennaio",
    "febbraio",
    "marzo",
    "aprile",
    "maggio",
    "giugno",
    "luglio",
    "agosto",
    "settembre",
    "ottobre",
    "novembre",
    "dicembre",
];
const PT: Months = &[
    "janeiro",
    "fevereiro",
    "março",
    "abril",
    "maio",
    "junho",
    "julho",
    "agosto",
    "setembro",
    "outubro",
    "novembro",
    "dezembro",
];
const FR: Months = &[
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];
/// Genitive, as in `15 января 2024 г.`.
const RU: Months = &[
    "января",
    "февраля",
    "марта",
    "апреля",
    "мая",
    "июня",
    "июля",
    "августа",
    "сентября",
    "октября",
    "ноября",
    "декабря",
];
/// Genitive.
const UK: Months = &[
    "січня",
    "лютого",
    "березня",
    "квітня",
    "травня",
    "червня",
    "липня",
    "серпня",
    "вересня",
    "жовтня",
    "листопада",
    "грудня",
];
/// Genitive.
const PL: Months = &[
    "stycznia",
    "lutego",
    "marca",
    "kwietnia",
    "maja",
    "czerwca",
    "lipca",
    "sierpnia",
    "września",
    "października",
    "listopada",
    "grudnia",
];
/// Genitive.
const CS: Months = &[
    "ledna",
    "února",
    "března",
    "dubna",
    "května",
    "června",
    "července",
    "srpna",
    "září",
    "října",
    "listopadu",
    "prosince",
];
const AR: Months = &[
    "يناير",
    "فبراير",
    "مارس",
    "أبريل",
    "مايو",
    "يونيو",
    "يوليو",
    "أغسطس",
    "سبتمبر",
    "أكتوبر",
    "نوفمبر",
    "ديسمبر",
];
const HE: Months = &[
    "ינואר",
    "פברואר",
    "מרץ",
    "אפריל",
    "מאי",
    "יוני",
    "יולי",
    "אוגוסט",
    "ספטמבר",
    "אוקטובר",
    "נובמבר",
    "דצמבר",
];
const JA_ZH: Months = &[
    "1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月",
];
const KO: Months = &[
    "1월", "2월", "3월", "4월", "5월", "6월", "7월", "8월", "9월", "10월", "11월", "12월",
];
const TR: Months = &[
    "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül", "Ekim",
    "Kasım", "Aralık",
];
const HI: Months = &[
    "जनवरी",
    "फ़रवरी",
    "मार्च",
    "अप्रैल",
    "मई",
    "जून",
    "जुलाई",
    "अगस्त",
    "सितंबर",
    "अक्तूबर",
    "नवंबर",
    "दिसंबर",
];
