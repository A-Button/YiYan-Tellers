mod corpus;

pub use corpus::{Corpus, LoadError};

use rand::prelude::IndexedRandom;
use rand::Rng;
use serde::Deserialize;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Deserialize, Clone)]
pub struct Sentence {
    pub front: String,
    pub behind: String,
    #[allow(dead_code)]
    pub r#type: String,
    #[allow(dead_code)]
    pub from: String,
    #[allow(dead_code)]
    pub length: u32,
}

/// 用户主目录：优先 `$HOME`，没有时退回 `getpwuid`。
pub fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home));
    }

    #[cfg(unix)]
    // SAFETY: 单线程启动阶段调用；只在 HOME 未设置时走这里。
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() {
            return None;
        }
        let dir = (*entry).pw_dir;
        if dir.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(dir).to_str().ok().map(PathBuf::from)
    }
    #[cfg(not(unix))]
    None
}

pub fn get_config_path() -> PathBuf {
    if let Some(path) = env::var_os("YYTS_SENTENCES") {
        PathBuf::from(path)
    } else {
        let mut home = home_dir().expect("无法获取用户主目录");
        home.push(".config/yyts/sentences.json");
        home
    }
}

pub fn read_sentences() -> Result<Vec<Sentence>, Box<dyn Error>> {
    read_sentences_from(&get_config_path())
}

pub fn read_sentences_from(path: &Path) -> Result<Vec<Sentence>, Box<dyn Error>> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("无法读取配置文件 {}: {}", path.display(), e))?;

    let sentences: Vec<Sentence> = serde_json::from_str(&content)?;
    Ok(sentences)
}

pub fn format_sentence(sentence: &Sentence) -> String {
    format_parts(&sentence.front, &sentence.behind, &sentence.from)
}

/// 复用的一段空格，避免为对齐填充反复分配。
const SPACES: &str = "                                                                ";

/// 把排版直接写进 `out`，返回写入的字节数；空间不足返回 `None`。
///
/// 这是唯一的排版实现，`format_parts` 和缓存层都复用它。
pub(crate) fn format_into(front: &str, behind: &str, from: &str, out: &mut [u8]) -> Option<usize> {
    let mut written = 0usize;

    if behind.is_empty() {
        // 单行句子：右引号直接跟在正文后面。
        put(out, &mut written, "「".as_bytes())?;
        put(out, &mut written, front.as_bytes())?;
        put(out, &mut written, "」  ————《".as_bytes())?;
        put(out, &mut written, from.as_bytes())?;
        put(out, &mut written, "》".as_bytes())?;
        return Some(written);
    }

    // UTF-8 里只有续字节以 10xxxxxx 开头，统计非续字节即等于字符数。
    let front_len = count_chars(front);
    let behind_len = count_chars(behind);
    let max_len = front_len.max(behind_len);

    let current_line_len = 8 + behind_len;
    let quote_position = max_len + 1; // +1 因为左引号已经占了一个位置
    let mut padding = quote_position.saturating_sub(current_line_len);

    put(out, &mut written, "「".as_bytes())?;
    put(out, &mut written, front.as_bytes())?;
    put(out, &mut written, b"\n")?;
    put(out, &mut written, b"        ")?;
    put(out, &mut written, behind.as_bytes())?;

    while padding >= SPACES.len() {
        put(out, &mut written, SPACES.as_bytes())?;
        padding -= SPACES.len();
    }
    put(out, &mut written, &SPACES.as_bytes()[..padding])?;

    put(out, &mut written, "」".as_bytes())?;
    put(out, &mut written, "  ————《".as_bytes())?;
    put(out, &mut written, from.as_bytes())?;
    put(out, &mut written, "》".as_bytes())?;
    Some(written)
}

#[inline]
fn put(out: &mut [u8], at: &mut usize, bytes: &[u8]) -> Option<()> {
    let end = at.checked_add(bytes.len())?;
    out.get_mut(*at..end)?.copy_from_slice(bytes);
    *at = end;
    Some(())
}

/// 两行/单行排版，返回 `String`（容量按最坏情况预留，只分配一次）。
pub(crate) fn format_parts(front: &str, behind: &str, from: &str) -> String {
    let capacity =
        64 + front.len() + behind.len() + from.len() + front.len().max(behind.len());
    let mut buffer = vec![0u8; capacity];
    let written = format_into(front, behind, from, &mut buffer).expect("容量已按最坏情况预留");
    buffer.truncate(written);

    // SAFETY: `format_into` 只写入合法的 UTF-8 片段。
    unsafe { String::from_utf8_unchecked(buffer) }
}

/// 排版中固定装饰的显示宽度。按 CJK 终端保守估计：引号、书名号、破折号都按 2 列算。
const QUOTE_W: usize = 2; // 「 」
const BOOK_W: usize = 2; // 《 》
const DASH_RUN_W: usize = 8; // 4 个 —（保守按 2 列）
const SEP_W: usize = 2; // 引号后的两个空格
const INDENT_W: usize = 8; // 第二行缩进

/// 渲染一行/两行后最宽那一行的显示列数（只计算，不分配）。
pub(crate) fn measured_width(front: &str, behind: &str, from: &str) -> usize {
    let (front_w, front_chars) = scan_width(front);
    let (from_w, _) = scan_width(from);

    if behind.is_empty() {
        return QUOTE_W + front_w + QUOTE_W + SEP_W + DASH_RUN_W + BOOK_W + from_w + BOOK_W;
    }

    let (behind_w, behind_chars) = scan_width(behind);
    let max_chars = front_chars.max(behind_chars);
    let pad = (max_chars + 1).saturating_sub(INDENT_W + behind_chars);

    let line1 = QUOTE_W + front_w;
    let line2 = INDENT_W
        + behind_w
        + pad
        + QUOTE_W
        + SEP_W
        + DASH_RUN_W
        + BOOK_W
        + from_w
        + BOOK_W;
    line1.max(line2)
}

/// 一遍扫描同时得到显示宽度和字符数，避免对同一字符串走两趟。
#[inline]
fn scan_width(text: &str) -> (usize, usize) {
    let bytes = text.as_bytes();
    let mut width = 0usize;
    let mut chars = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b0 = bytes[i];
        if b0 < 0x80 {
            chars += 1;
            if (0x20..0x7F).contains(&b0) {
                width += 1;
            }
            i += 1;
        } else if (0xE4..=0xE9).contains(&b0) {
            // U+4000..=U+9FFF（CJK 统一表意文字及扩展 A 核心区）全部为 3 字节 UTF-8 且显示宽度为 2。
            chars += 1;
            width += 2;
            i += 3;
        } else {
            let mut iter = text[i..].chars();
            if let Some(ch) = iter.next() {
                chars += 1;
                width += UnicodeWidthChar::width(ch).unwrap_or(0);
                i += ch.len_utf8();
            } else {
                break;
            }
        }
    }
    (width, chars)
}

/// 当前终端宽度（列）。
///
/// 优先级：`YYTS_WIDTH` 环境变量（0 表示关闭宽度过滤）→ stdout/stderr 的
/// `TIOCGWINSZ` → `COLUMNS`。都拿不到时返回 `None`，此时不做宽度过滤。
pub fn terminal_width() -> Option<usize> {
    if let Some(value) = std::env::var_os("YYTS_WIDTH") {
        let width = value.to_string_lossy().trim().parse::<usize>().ok()?;
        return if width == 0 { None } else { Some(width) };
    }

    #[cfg(unix)]
    if let Some(width) = tty_width(libc::STDOUT_FILENO).or_else(|| tty_width(libc::STDERR_FILENO)) {
        return Some(width);
    }

    std::env::var_os("COLUMNS")
        .and_then(|value| value.to_string_lossy().trim().parse::<usize>().ok())
        .filter(|width| *width > 0)
}

#[cfg(unix)]
fn tty_width(fd: libc::c_int) -> Option<usize> {
    // SAFETY: TIOCGWINSZ 只会写入 winsize 结构体，fd 是标准流的描述符。
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) == 0 && size.ws_col > 0 {
            return Some(size.ws_col as usize);
        }
    }
    None
}

#[cfg(not(unix))]
fn tty_width(_fd: i32) -> Option<usize> {
    None
}

#[inline]
fn count_chars(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0usize;
    let (chunks, remainder) = bytes.as_chunks::<8>();
    for &chunk in chunks {
        let word = u64::from_ne_bytes(chunk);
        // UTF-8 续字节形如 10xxxxxx：最高位为 1 且次高位为 0。
        let cont = (word & 0x8080_8080_8080_8080) & !(word << 1);
        count += 8 - (cont.count_ones() as usize);
    }
    for &byte in remainder {
        count += usize::from((byte & 0xC0) != 0x80);
    }
    count
}

/// 每次运行只需要一次随机性：用时钟 + pid 做种子，避免熵源初始化开销。
pub fn entropy_seed() -> u64 {
    #[cfg(unix)]
    let nanos = unsafe {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) == 0 {
            (ts.tv_sec as u64)
                .wrapping_mul(1_000_000_000)
                .wrapping_add(ts.tv_nsec as u64)
        } else {
            0x9E37_79B9_7F4A_7C15
        }
    };
    #[cfg(not(unix))]
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let pid = u64::from(std::process::id());
    nanos ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (nanos >> 29)
}

/// 从给定的句子列表中随机选择一句并返回格式化后的字符串。
///
/// 该函数接受一个外部提供的 RNG，以便在基准测试中可以使用确定性 RNG（如 StdRng）。
pub fn pick_and_format_with_rng<R: Rng + ?Sized>(
    sentences: &[Sentence],
    rng: &mut R,
) -> Option<String> {
    sentences
        .choose(rng)
        .map(|sentence| format_parts(&sentence.front, &sentence.behind, &sentence.from))
}

/// 完整的一次运行：读配置（或缓存）、随机选一句、格式化。
///
/// 返回 `Ok(None)` 表示句子库为空。
pub fn run_once() -> Result<Option<String>, Box<dyn Error>> {
    let corpus = Corpus::load(&get_config_path())?;
    if corpus.is_empty() {
        return Ok(None);
    }
    let index = corpus
        .pick_index_fitting(entropy_seed(), terminal_width())
        .ok_or("句子索引无效")?;
    Ok(corpus.format_at(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 优化前的实现，逐字节对照用。
    #[allow(clippy::single_char_add_str)]
    fn legacy_format(sentence: &Sentence) -> String {
        let front_len = sentence.front.chars().count();
        let behind_len = sentence.behind.chars().count();
        let max_len = front_len.max(behind_len);
        let spaces = "        ";

        let mut output = String::new();
        output.push('「');
        output.push_str(&sentence.front);
        output.push('\n');
        output.push_str(spaces);
        output.push_str(&sentence.behind);

        let current_line_len = spaces.chars().count() + behind_len;
        let quote_position = max_len + 1;

        if current_line_len < quote_position {
            let padding = quote_position - current_line_len;
            output.push_str(&" ".repeat(padding));
        }

        output.push_str("」");
        output.push_str("  ————《");
        output.push_str(&sentence.from);
        output.push_str("》");
        output
    }

    /// 单行句子的预期格式。
    fn single_line(front: &str, from: &str) -> String {
        format!("「{front}」  ————《{from}》")
    }

    fn sentence(front: &str, behind: &str, from: &str) -> Sentence {
        Sentence {
            front: front.to_string(),
            behind: behind.to_string(),
            r#type: "t".to_string(),
            from: from.to_string(),
            length: 0,
        }
    }

    #[test]
    fn format_matches_legacy() {
        let cases = [
            ("这是一个较长的前半句，用来测试字符串拼接性能。", "短后半句", "来源"),
            ("短", "这是一个较长的后半句，用来测试字符串拼接性能。", "来源"),
            ("长度相同", "长度相同", "来源"),
            ("", "", ""),
            ("只有前半句", "", "无后半"),
            ("", "只有后半句", "无前半"),
            ("a", "bb", "c"),
            ("🎉🎉🎉", "中文", "混合"),
            ("  带空格  ", "  ", "  出处  "),
            ("很长很长很长很长很长很长很长很长很长很长很长很长很长很长很长", "短", "长"),
        ];

        for (front, behind, from) in cases {
            let s = sentence(front, behind, from);
            let expected = if behind.is_empty() {
                single_line(front, from)
            } else {
                legacy_format(&s)
            };
            assert_eq!(format_sentence(&s), expected, "case: {front:?} / {behind:?}");
        }

        // 随机长度组合，避免只覆盖手写用例
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |limit: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state as usize) % (limit + 1)
        };

        for _ in 0..200 {
            let front_len = next(40);
            let front: String = (0..front_len)
                .map(|_| ['a', '中', '🎉', ' '][next(3)])
                .collect();
            let behind_len = next(40);
            let behind: String = (0..behind_len)
                .map(|_| ['b', '文', '🚀', ' '][next(3)])
                .collect();
            let from_len = next(10);
            let from: String = (0..from_len).map(|_| ['c', '源'][next(1)]).collect();

            let s = sentence(&front, &behind, &from);
            let expected = if behind.is_empty() {
                single_line(&front, &from)
            } else {
                legacy_format(&s)
            };
            assert_eq!(format_sentence(&s), expected, "random case");
        }
    }

    #[test]
    fn format_into_matches_format_parts() {
        let cases = [
            ("进攻D点！", "", "战争雷霆"),
            ("众里寻他千百度，蓦然回首，", "那人却在，灯火阑珊处。", "青玉案·元夕"),
            ("", "", ""),
            ("a", "bb", "c"),
            ("很长很长很长很长很长很长很长很长很长很长很长很长很长很长", "短", "出处"),
        ];

        for (front, behind, from) in cases {
            let expected = format_parts(front, behind, from);
            let mut buffer = [0u8; 4096];
            let written = format_into(front, behind, from, &mut buffer).unwrap();
            assert_eq!(&buffer[..written], expected.as_bytes());
        }

        let mut tiny = [0u8; 5];
        assert!(format_into("很长的句子", "", "出处", &mut tiny).is_none());
    }

    #[test]
    fn single_line_without_behind() {
        let s = sentence("孤舟蓑笠翁，独钓寒江雪。", "", "江雪");
        assert_eq!(
            format_sentence(&s),
            "「孤舟蓑笠翁，独钓寒江雪。」  ————《江雪》"
        );

        let empty = sentence("", "", "无");
        assert_eq!(format_sentence(&empty), "「」  ————《无》");
    }

    #[test]
    fn measured_width_covers_rendering() {
        for cp in 0x4000u32..=0x9FFFu32 {
            let ch = char::from_u32(cp).unwrap();
            assert_eq!(UnicodeWidthChar::width(ch), Some(2), "codepoint U+{cp:04X}");
        }
        let cases = [
            ("进攻D点！", "", "战争雷霆"),
            ("众里寻他千百度，蓦然回首，", "那人却在，灯火阑珊处。", "青玉案·元夕"),
            ("a", "b", "f"),
            ("很长很长的前半句很长很长的前半句", "短", "出处"),
            ("短", "很长很长的后半句很长很长的后半句", "出处"),
        ];
        for (front, behind, from) in cases {
            let rendered = format_sentence(&sentence(front, behind, from));
            let actual = rendered
                .lines()
                .map(unicode_width::UnicodeWidthStr::width)
                .max()
                .unwrap();
            let measured = measured_width(front, behind, from);
            assert!(measured >= actual, "measured {measured} < actual {actual}");
            // 4 个破折号按 2 列保守估计，最多比实际宽 4 列
            assert!(measured - actual <= 4, "measured {measured} actual {actual}");
        }
    }

    #[test]
    fn count_chars_matches_std() {
        for text in ["", "abc", "中文", "🎉", "é", "a中文🎉 ", "  ", "\u{200b}"] {
            assert_eq!(count_chars(text), text.chars().count(), "case: {text:?}");
        }
    }

    #[test]
    fn terminal_width_override() {
        // SAFETY: 该测试独占这两个环境变量，其他测试不读取它们。
        unsafe { std::env::set_var("YYTS_WIDTH", "42") };
        assert_eq!(terminal_width(), Some(42));
        unsafe { std::env::set_var("YYTS_WIDTH", "0") };
        assert_eq!(terminal_width(), None);
        unsafe { std::env::remove_var("YYTS_WIDTH") };
    }

    #[test]
    fn entropy_seed_varies() {
        let mut seeds = std::collections::HashSet::new();
        for _ in 0..64 {
            seeds.insert(entropy_seed());
        }
        assert!(seeds.len() > 32, "种子区分度太低: {}", seeds.len());
    }

    #[test]
    fn pick_is_within_range() {
        let sentences = vec![sentence("a", "b", "c"), sentence("d", "e", "f")];
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        use rand::SeedableRng;

        for _ in 0..100 {
            let picked = pick_and_format_with_rng(&sentences, &mut rng).unwrap();
            assert!(picked.contains('a') || picked.contains('d'));
        }
    }

    #[test]
    fn home_dir_uses_env() {
        // SAFETY: 该测试独占 HOME，其他测试不读取它。
        unsafe { std::env::set_var("HOME", "/tmp/yyts-home-test") };
        assert_eq!(home_dir(), Some(PathBuf::from("/tmp/yyts-home-test")));
        unsafe { std::env::remove_var("HOME") };
    }
}
