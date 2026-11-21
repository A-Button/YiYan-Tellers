//! 句子库缓存。
//!
//! 缓存布局（V4）：
//!
//! ```text
//! [64 B 头部][n × 24 B 记录][n × 2 B 宽度表][文本池]
//! ```
//!
//! 记录按渲染宽度升序排列，宽度表用于二分出「放得下」的前缀长度。
//! 命中缓存时只 `pread` 64 B 头部、一条记录和它的连续文本（带宽度过滤时再读紧凑宽度表），
//! 省掉 `mmap` 建立 VMA、缺页中断与 `munmap` 拆页表的内核开销。

use crate::format_into as write_formatted;
use crate::format_parts;
use rand::Rng;
use serde::Deserialize;
use std::borrow::Cow;
use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"YYTSC001";
const VERSION: u32 = 4;
const HEADER_LEN: usize = 64;
/// 每条记录 24 字节：front/behind/from 各一对 (offset,len)。
const RECORD_LEN: usize = 24;
const NO_CACHE_ENV: &str = "YYTS_NO_CACHE";
const CACHE_DIR_ENV: &str = "YYTS_CACHE_DIR";
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const MAX_WIDTH: usize = u16::MAX as usize;

#[derive(Debug)]
pub enum LoadError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Json(serde_json::Error),
    Format(&'static str),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io { path, source } => {
                write!(f, "无法读取配置文件 {}: {}", path.display(), source)
            }
            LoadError::Json(source) => fmt::Display::fmt(source, f),
            LoadError::Format(message) => f.write_str(message),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            LoadError::Io { source, .. } => Some(source),
            LoadError::Json(source) => Some(source),
            LoadError::Format(_) => None,
        }
    }
}

impl From<serde_json::Error> for LoadError {
    fn from(source: serde_json::Error) -> Self {
        LoadError::Json(source)
    }
}

#[derive(Deserialize)]
struct SentenceRef<'a> {
    #[serde(borrow)]
    front: Cow<'a, str>,
    #[serde(borrow)]
    behind: Cow<'a, str>,
    #[serde(borrow)]
    #[allow(dead_code)]
    r#type: Cow<'a, str>,
    #[serde(borrow)]
    from: Cow<'a, str>,
    #[allow(dead_code)]
    length: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceMeta {
    size: u64,
    mtime_s: i64,
    mtime_ns: u32,
}

impl SourceMeta {
    #[cfg(unix)]
    fn from_path(path: &Path) -> Option<Self> {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        let mut c_path = [0u8; 4096];
        if bytes.len() + 1 > c_path.len() || bytes.contains(&0) {
            return Self::from_path_std(path);
        }
        c_path[..bytes.len()].copy_from_slice(bytes);
        c_path[bytes.len()] = 0;
        // SAFETY: `c_path` 以 `\0` 结尾，且 `stat` 结构体由内核填充。
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::stat(c_path.as_ptr().cast(), &mut st) != 0 || st.st_mtime < 0 {
                return None;
            }
            Some(Self {
                size: st.st_size as u64,
                mtime_s: st.st_mtime,
                mtime_ns: st.st_mtime_nsec as u32,
            })
        }
    }

    #[cfg(not(unix))]
    fn from_path(path: &Path) -> Option<Self> {
        Self::from_path_std(path)
    }

    fn from_path_std(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(Self {
            size: meta.len(),
            mtime_s: mtime.as_secs() as i64,
            mtime_ns: mtime.subsec_nanos(),
        })
    }
}

#[cfg(unix)]
use std::os::unix::fs::FileExt;

enum Backing {
    Owned(Vec<u8>),
    #[cfg(not(unix))]
    Mapped(memmap2::Mmap),
    #[cfg(unix)]
    CachedFile {
        file: File,
        cache_path: PathBuf,
        meta: SourceMeta,
    },
}

/// 已经切成记录表的句子库。
pub struct Corpus {
    data: Backing,
    count: usize,
    blob_off: usize,
    width_cdf: [u32; 256],
}

impl Corpus {
    /// 解析 JSON 并在内存里构建缓存布局（不落盘）。
    pub fn parse(json: &[u8]) -> Result<Self, LoadError> {
        Self::parse_inner(json, None)
    }

    /// 优先读缓存；缓存缺失或过期时解析 JSON 并重建缓存。
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let meta = SourceMeta::from_path(path);
        let cache_enabled = std::env::var_os(NO_CACHE_ENV).is_none();

        if cache_enabled
            && let (Some(meta), Some(cache)) = (meta.as_ref(), cache_path(path))
            && let Some(corpus) = Self::read_cache(&cache, meta)
        {
            return Ok(corpus);
        }

        let file = File::open(path).map_err(|source| LoadError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        // SAFETY: 源文件只读映射；解析期间调用方不会改写它。
        let corpus = match unsafe { memmap2::Mmap::map(&file) } {
            Ok(map) => Self::parse_inner(&map, meta.as_ref())?,
            Err(_) => {
                let bytes = fs::read(path).map_err(|source| LoadError::Io {
                    path: path.to_path_buf(),
                    source,
                })?;
                Self::parse_inner(&bytes, meta.as_ref())?
            }
        };

        if cache_enabled
            && meta.is_some()
            && let Some(cache) = cache_path(path)
            && let Backing::Owned(bytes) = &corpus.data
        {
            let _ = write_cache(&cache, bytes, &corpus.width_cdf);
        }

        Ok(corpus)
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 均匀随机地选一个下标（用于通用 API / 测试）。
    #[inline]
    pub fn pick_index<R: Rng + ?Sized>(&self, rng: &mut R) -> Option<usize> {
        if self.count == 0 {
            None
        } else {
            Some(rng.random_range(0..self.count))
        }
    }

    /// 取第 `idx` 条的 (front, behind, from)。会分配三个字符串，主要用于测试/工具。
    pub fn sentence_at(&self, idx: usize) -> Option<(String, String, String)> {
        self.with_text_at(idx, |front, behind, from| {
            (front.to_owned(), behind.to_owned(), from.to_owned())
        })
    }

    /// 第 `idx` 条的渲染宽度（建缓存时预计算）。
    #[inline]
    pub fn max_width_at(&self, idx: usize) -> Option<usize> {
        if idx >= self.count {
            return None;
        }
        let at = widths_off(self.count) + idx * 2;
        match &self.data {
            Backing::Owned(bytes) => {
                let slice = bytes.get(at..at + 2)?;
                Some(u16::from_le_bytes(slice.try_into().ok()?) as usize)
            }
            #[cfg(not(unix))]
            Backing::Mapped(map) => {
                let slice = map.get(at..at + 2)?;
                Some(u16::from_le_bytes(slice.try_into().ok()?) as usize)
            }
            #[cfg(unix)]
            Backing::CachedFile { file, .. } => {
                let mut buf = [0u8; 2];
                file.read_exact_at(&mut buf, at as u64).ok()?;
                Some(u16::from_le_bytes(buf) as usize)
            }
        }
    }

    /// 把第 `idx` 条格式化进 `out`，返回写入字节数；空间不足返回 `None`。
    pub fn format_into(&self, idx: usize, out: &mut [u8]) -> Option<usize> {
        self.with_text_at(idx, |front, behind, from| {
            write_formatted(front, behind, from, out)
        })?
    }

    /// 把第 `idx` 条格式化成 `String`。
    pub fn format_at(&self, idx: usize) -> Option<String> {
        self.with_text_at(idx, format_parts)
    }

    /// 随机选一句并格式化。
    pub fn pick_and_format_with_rng<R: Rng + ?Sized>(&self, rng: &mut R) -> Option<String> {
        let idx = self.pick_index(rng)?;
        self.format_at(idx)
    }

    /// 放得下的句子数量：宽度表升序，优先查 0..256 前缀表，未预热或超宽时二分出 `width <= limit` 的前缀长度。
    #[inline(always)]
    fn fitting_count(&self, limit: usize) -> usize {
        if limit < 256 {
            let cached = self.width_cdf[limit];
            if cached != u32::MAX {
                return cached as usize;
            }
        }
        self.fitting_count_slow(limit)
    }

    #[inline(never)]
    fn fitting_count_slow(&self, limit: usize) -> usize {
        let count = self.count;
        if count == 0 {
            return 0;
        }
        let limit_u16 = limit.min(u16::MAX as usize) as u16;
        match &self.data {
            Backing::Owned(bytes) => {
                let ptr = unsafe { bytes.as_ptr().add(widths_off(count)) as *const u16 };
                branchless_lower_bound(ptr, count, limit_u16)
            }
            #[cfg(not(unix))]
            Backing::Mapped(map) => {
                let ptr = unsafe { map.as_ptr().add(widths_off(count)) as *const u16 };
                branchless_lower_bound(ptr, count, limit_u16)
            }
            #[cfg(unix)]
            Backing::CachedFile {
                file,
                cache_path,
                meta,
            } => {
                if limit < 256
                    && let Some(val) = read_cdf_sidecar(cache_path, meta, self.count, limit)
                {
                    return val;
                }
                // 边车文件未命中（如旧版 V4 缓存且缓存目录不可写，或 limit >= 256）：
                // 先按 1024 项（2 KiB）分界做几次 2 字节 pread，再一次 pread 读入对应 2 KiB 区间在栈上无分支二分，
                // 无需拷贝整张 21 KiB 宽度表。
                let base_off = widths_off(count) as u64;
                let mut low = 0usize;
                let mut high = count;
                while high - low > 1024 {
                    let mid = (low + high) / 2;
                    let mut buf = [0u8; 2];
                    if file.read_exact_at(&mut buf, base_off + (mid * 2) as u64).is_err() {
                        return 0;
                    }
                    if u16::from_le_bytes(buf) <= limit_u16 {
                        low = mid + 1;
                    } else {
                        high = mid;
                    }
                }
                let rem = high - low;
                if rem == 0 {
                    return low;
                }
                let mut stack_buf = [0u8; 2048];
                let slice = &mut stack_buf[..rem * 2];
                if file.read_exact_at(slice, base_off + (low * 2) as u64).is_err() {
                    return 0;
                }
                let ptr = slice.as_ptr() as *const u16;
                low + branchless_lower_bound(ptr, rem, limit_u16)
            }
        }
    }

    /// 用种子选一个能放下的下标（主路径：不依赖 `rand`，避免熵源初始化开销）。
    ///
    /// 记录按宽度升序：取前缀长度 k，再在 `[0, k)` 上做一次均匀映射
    /// （Lemire 的 multiply-shift，无模偏差）。`None` 表示不做宽度过滤；
    /// 全都放不下时返回最窄的第 0 条。
    pub fn pick_index_fitting(&self, seed: u64, max_width: Option<usize>) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        let fits = match max_width {
            None => self.count,
            Some(limit) => {
                let fits = self.fitting_count(limit);
                if fits == 0 {
                    return Some(0);
                }
                fits
            }
        };
        let mut state = seed ^ (self.count as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let random = splitmix64(&mut state);
        Some(((u128::from(random) * fits as u128) >> 64) as usize)
    }

    /// 随机选一句，且保证渲染宽度不超过 `max_width`（随机源换成 `Rng`，测试/基准用）。
    pub fn pick_fitting_with_rng<R: Rng + ?Sized>(
        &self,
        rng: &mut R,
        max_width: Option<usize>,
    ) -> Option<String> {
        let Some(limit) = max_width else {
            return self.pick_and_format_with_rng(rng);
        };
        if self.count == 0 {
            return None;
        }
        let fits = self.fitting_count(limit);
        if fits == 0 {
            return self.format_at(0);
        }
        self.format_at(rng.random_range(0..fits))
    }

    #[inline]
    fn with_text_at<R, F: FnOnce(&str, &str, &str) -> R>(&self, idx: usize, f: F) -> Option<R> {
        if idx >= self.count {
            return None;
        }
        let record_at = HEADER_LEN + idx * RECORD_LEN;
        match &self.data {
            Backing::Owned(bytes) => {
                // 不变量：`Backing::Owned` 仅由 `parse_inner` 从已校验 UTF-8 的 `&str`（`SentenceRef`）经 `encode_prepared`
                // 构造，不接受任何外部字节缓冲区。这里仍对 `record` 与 `blob` 做边界检查，但省去重复的 UTF-8 扫描。
                let record = bytes.get(record_at..record_at + RECORD_LEN)?;
                let f_off = u32::from_le_bytes(record[0..4].try_into().ok()?) as usize;
                let f_len = u32::from_le_bytes(record[4..8].try_into().ok()?) as usize;
                let b_off = u32::from_le_bytes(record[8..12].try_into().ok()?) as usize;
                let b_len = u32::from_le_bytes(record[12..16].try_into().ok()?) as usize;
                let m_off = u32::from_le_bytes(record[16..20].try_into().ok()?) as usize;
                let m_len = u32::from_le_bytes(record[20..24].try_into().ok()?) as usize;
                let blob = bytes.get(self.blob_off..)?;
                let f_slice = blob.get(f_off..f_off.checked_add(f_len)?)?;
                let b_slice = blob.get(b_off..b_off.checked_add(b_len)?)?;
                let m_slice = blob.get(m_off..m_off.checked_add(m_len)?)?;
                debug_assert!(std::str::from_utf8(f_slice).is_ok());
                debug_assert!(std::str::from_utf8(b_slice).is_ok());
                debug_assert!(std::str::from_utf8(m_slice).is_ok());
                // SAFETY: 见上方不变量说明，`f_slice`/`b_slice`/`m_slice` 均直接拷贝自有效 `&str::as_bytes()`。
                unsafe {
                    Some(f(
                        std::str::from_utf8_unchecked(f_slice),
                        std::str::from_utf8_unchecked(b_slice),
                        std::str::from_utf8_unchecked(m_slice),
                    ))
                }
            }
            #[cfg(not(unix))]
            Backing::Mapped(map) => {
                let record = map.get(record_at..record_at + RECORD_LEN)?;
                let f_off = u32::from_le_bytes(record[0..4].try_into().ok()?) as usize;
                let f_len = u32::from_le_bytes(record[4..8].try_into().ok()?) as usize;
                let b_off = u32::from_le_bytes(record[8..12].try_into().ok()?) as usize;
                let b_len = u32::from_le_bytes(record[12..16].try_into().ok()?) as usize;
                let m_off = u32::from_le_bytes(record[16..20].try_into().ok()?) as usize;
                let m_len = u32::from_le_bytes(record[20..24].try_into().ok()?) as usize;
                let blob = map.get(self.blob_off..)?;
                Some(f(
                    std::str::from_utf8(blob.get(f_off..f_off.checked_add(f_len)?)?).ok()?,
                    std::str::from_utf8(blob.get(b_off..b_off.checked_add(b_len)?)?).ok()?,
                    std::str::from_utf8(blob.get(m_off..m_off.checked_add(m_len)?)?).ok()?,
                ))
            }
            #[cfg(unix)]
            Backing::CachedFile { file, .. } => {
                let mut record = [0u8; RECORD_LEN];
                file.read_exact_at(&mut record, record_at as u64).ok()?;
                let f_off = u32::from_le_bytes(record[0..4].try_into().ok()?) as usize;
                let f_len = u32::from_le_bytes(record[4..8].try_into().ok()?) as usize;
                let b_off = u32::from_le_bytes(record[8..12].try_into().ok()?) as usize;
                let b_len = u32::from_le_bytes(record[12..16].try_into().ok()?) as usize;
                let m_off = u32::from_le_bytes(record[16..20].try_into().ok()?) as usize;
                let m_len = u32::from_le_bytes(record[20..24].try_into().ok()?) as usize;

                // encode_prepared 中同一条句子的 front, behind, from 总是紧挨着顺序写入。
                let total = f_len.checked_add(b_len)?.checked_add(m_len)?;
                if b_off == f_off + f_len && m_off == b_off + b_len {
                    let file_off = (self.blob_off.checked_add(f_off)?) as u64;
                    let mut stack_buf = [0u8; 2048];
                    if total <= stack_buf.len() {
                        let slice = &mut stack_buf[..total];
                        file.read_exact_at(slice, file_off).ok()?;
                        return Some(f(
                            std::str::from_utf8(&slice[..f_len]).ok()?,
                            std::str::from_utf8(&slice[f_len..f_len + b_len]).ok()?,
                            std::str::from_utf8(&slice[f_len + b_len..]).ok()?,
                        ));
                    }
                    let mut vec_buf = vec![0u8; total];
                    file.read_exact_at(&mut vec_buf, file_off).ok()?;
                    return Some(f(
                        std::str::from_utf8(&vec_buf[..f_len]).ok()?,
                        std::str::from_utf8(&vec_buf[f_len..f_len + b_len]).ok()?,
                        std::str::from_utf8(&vec_buf[f_len + b_len..]).ok()?,
                    ));
                }

                let mut buf = vec![0u8; total];
                file.read_exact_at(&mut buf[..f_len], (self.blob_off.checked_add(f_off)?) as u64)
                    .ok()?;
                file.read_exact_at(
                    &mut buf[f_len..f_len + b_len],
                    (self.blob_off.checked_add(b_off)?) as u64,
                )
                .ok()?;
                file.read_exact_at(
                    &mut buf[f_len + b_len..],
                    (self.blob_off.checked_add(m_off)?) as u64,
                )
                .ok()?;
                Some(f(
                    std::str::from_utf8(&buf[..f_len]).ok()?,
                    std::str::from_utf8(&buf[f_len..f_len + b_len]).ok()?,
                    std::str::from_utf8(&buf[f_len + b_len..]).ok()?,
                ))
            }
        }
    }

    fn parse_inner(json: &[u8], meta: Option<&SourceMeta>) -> Result<Self, LoadError> {
        let entries: Vec<SentenceRef<'_>> = match std::str::from_utf8(json) {
            Ok(text) => serde_json::from_str(text)?,
            Err(_) => serde_json::from_slice(json)?,
        };
        let widths = widths_parallel(&entries);
        let (bytes, width_cdf) = encode_prepared(&entries, &widths, meta)?;
        let (count, blob_off) = decode_header(&bytes, bytes.len(), None)
            .expect("内部生成的缓存布局必然有效");
        Ok(Self {
            data: Backing::Owned(bytes),
            count,
            blob_off,
            width_cdf,
        })
    }

    #[cfg(unix)]
    fn read_cache(path: &Path, meta: &SourceMeta) -> Option<Self> {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::{AsRawFd, FromRawFd};
        let bytes = path.as_os_str().as_bytes();
        let file = if bytes.len() < 4096 && !bytes.contains(&0) {
            let mut c_path = [0u8; 4096];
            c_path[..bytes.len()].copy_from_slice(bytes);
            c_path[bytes.len()] = 0;
            // SAFETY: `c_path` 以 `\0` 结尾，只读打开并带 `O_CLOEXEC`。
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr().cast(),
                    libc::O_RDONLY | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return None;
            }
            unsafe { File::from_raw_fd(fd) }
        } else {
            File::open(path).ok()?
        };

        let mut header = [0u8; HEADER_LEN];
        file.read_exact_at(&mut header, 0).ok()?;
        // SAFETY: 用 `lseek(fd, 0, SEEK_END)` 获取缓存文件的真实字节长度，防止被截断的缓存绕过长度校验。
        let file_len = unsafe {
            let end = libc::lseek(file.as_raw_fd(), 0, libc::SEEK_END);
            if end < 0 {
                return None;
            }
            end as usize
        };
        let (count, blob_off) = decode_header(&header, file_len, Some(meta))?;
        Some(Self {
            data: Backing::CachedFile {
                file,
                cache_path: path.to_path_buf(),
                meta: *meta,
            },
            count,
            blob_off,
            width_cdf: [u32::MAX; 256],
        })
    }

    #[cfg(not(unix))]
    fn read_cache(path: &Path, meta: &SourceMeta) -> Option<Self> {
        let file = File::open(path).ok()?;
        // SAFETY: 缓存由本进程「临时文件 + rename」原子替换，映射到的 inode 不会被改写。
        let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        let (count, blob_off) = decode_header(&map, map.len(), Some(meta))?;
        Some(Self {
            data: Backing::Mapped(map),
            count,
            blob_off,
            width_cdf: [u32::MAX; 256],
        })
    }
}

#[inline]
fn branchless_lower_bound(ptr: *const u16, count: usize, limit: u16) -> usize {
    let mut base = 0usize;
    let mut len = count;
    while len > 1 {
        let half = len / 2;
        // SAFETY: `base + half < count`，调用方已保证 `ptr` 指向至少 `count` 个按小端存放的 `u16`。
        // 先以 `[u8; 2]` 读取再走 `u16::from_le_bytes`，在任意字节序与对齐下均正确。
        let raw = unsafe { *(ptr.add(base + half) as *const [u8; 2]) };
        let w = u16::from_le_bytes(raw);
        if w <= limit {
            base += half;
        }
        len -= half;
    }
    let raw0 = unsafe { *(ptr.add(base) as *const [u8; 2]) };
    let w0 = u16::from_le_bytes(raw0);
    base + usize::from(w0 <= limit)
}

#[inline]
fn widths_off(count: usize) -> usize {
    HEADER_LEN + count * RECORD_LEN
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 宽度计算之间互不依赖，按块分给多个线程（冷路径专用，小数据自动退回单线程）。
fn widths_parallel(entries: &[SentenceRef<'_>]) -> Vec<u16> {
    let mut widths = vec![0u16; entries.len()];
    let threads = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .min(8);

    if threads < 2 || entries.len() < 512 {
        for (entry, width) in entries.iter().zip(widths.iter_mut()) {
            *width = entry_width(entry);
        }
        return widths;
    }

    let chunk = entries.len().div_ceil(threads);
    std::thread::scope(|scope| {
        for (chunk_entries, chunk_widths) in entries.chunks(chunk).zip(widths.chunks_mut(chunk)) {
            scope.spawn(move || {
                for (entry, width) in chunk_entries.iter().zip(chunk_widths.iter_mut()) {
                    *width = entry_width(entry);
                }
            });
        }
    });
    widths
}

#[inline]
fn entry_width(entry: &SentenceRef<'_>) -> u16 {
    crate::measured_width(
        entry.front.as_ref(),
        entry.behind.as_ref(),
        entry.from.as_ref(),
    )
    .min(MAX_WIDTH) as u16
}

fn encode_prepared(
    entries: &[SentenceRef<'_>],
    widths: &[u16],
    meta: Option<&SourceMeta>,
) -> Result<(Vec<u8>, [u32; 256]), LoadError> {
    let count = entries.len();
    if count > u32::MAX as usize {
        return Err(LoadError::Format("句子数量超出缓存格式上限"));
    }

    let mut blob_len = 0usize;
    for entry in entries {
        blob_len += entry.front.len() + entry.behind.len() + entry.from.len();
    }
    if blob_len > u32::MAX as usize {
        return Err(LoadError::Format("句子数据超出缓存格式上限"));
    }

    // 按渲染宽度升序（同宽保持原顺序）。宽度范围很小，用计数排序 O(n)。
    let (order, cdf) = order_by_width(widths);

    let blob_off = widths_off(count) + count * 2;
    let mut out = vec![0u8; blob_off + blob_len];

    out[0..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[16..24].copy_from_slice(&(count as u64).to_le_bytes());
    if let Some(meta) = meta {
        out[24..32].copy_from_slice(&meta.size.to_le_bytes());
        out[32..40].copy_from_slice(&meta.mtime_s.to_le_bytes());
        out[40..44].copy_from_slice(&meta.mtime_ns.to_le_bytes());
    }
    out[48..56].copy_from_slice(&(blob_off as u64).to_le_bytes());
    out[56..64].copy_from_slice(&(blob_len as u64).to_le_bytes());

    let mut position = 0usize;
    for (rank, &original) in order.iter().enumerate() {
        let entry = &entries[original as usize];
        let record = HEADER_LEN + rank * RECORD_LEN;
        let fields = [
            entry.front.as_ref(),
            entry.behind.as_ref(),
            entry.from.as_ref(),
        ];
        for (slot, text) in fields.iter().enumerate() {
            let at = record + slot * 8;
            out[at..at + 4].copy_from_slice(&(position as u32).to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&(text.len() as u32).to_le_bytes());
            out[blob_off + position..blob_off + position + text.len()]
                .copy_from_slice(text.as_bytes());
            position += text.len();
        }
        let width_at = widths_off(count) + rank * 2;
        out[width_at..width_at + 2].copy_from_slice(&widths[original as usize].to_le_bytes());
    }

    Ok((out, cdf))
}

/// 计数排序：返回「按宽度升序」的原始下标以及 0..256 的宽度前缀累计表。
fn order_by_width(widths: &[u16]) -> (Vec<u32>, [u32; 256]) {
    let max_width = widths.iter().copied().max().unwrap_or(0) as usize;
    let table_len = (max_width + 2).max(257);
    let mut starts = vec![0u32; table_len];
    for &width in widths {
        starts[width as usize + 1] += 1;
    }
    for index in 1..starts.len() {
        starts[index] += starts[index - 1];
    }

    let mut cdf = [0u32; 256];
    for (w, slot) in cdf.iter_mut().enumerate() {
        *slot = starts[w + 1];
    }

    let mut order = vec![0u32; widths.len()];
    for (index, &width) in widths.iter().enumerate() {
        let slot = &mut starts[width as usize];
        order[*slot as usize] = index as u32;
        *slot += 1;
    }
    (order, cdf)
}

/// 校验头部；`meta` 为 `None` 时跳过源文件信息比对（用于刚生成的内存数据）。
fn decode_header(data: &[u8], file_len: usize, meta: Option<&SourceMeta>) -> Option<(usize, usize)> {
    if data.len() < HEADER_LEN || !data.starts_with(MAGIC) {
        return None;
    }
    if u32::from_le_bytes(data[8..12].try_into().ok()?) != VERSION {
        return None;
    }
    let count = u64::from_le_bytes(data[16..24].try_into().ok()?) as usize;
    if count > u32::MAX as usize {
        return None;
    }
    if let Some(meta) = meta {
        let size = u64::from_le_bytes(data[24..32].try_into().ok()?);
        let mtime_s = i64::from_le_bytes(data[32..40].try_into().ok()?);
        let mtime_ns = u32::from_le_bytes(data[40..44].try_into().ok()?);
        if size != meta.size || mtime_s != meta.mtime_s || mtime_ns != meta.mtime_ns {
            return None;
        }
    }
    let blob_off = u64::from_le_bytes(data[48..56].try_into().ok()?) as usize;
    let blob_len = u64::from_le_bytes(data[56..64].try_into().ok()?) as usize;
    if blob_off != widths_off(count).checked_add(count.checked_mul(2)?)? {
        return None;
    }
    if blob_off.checked_add(blob_len)? != file_len {
        return None;
    }
    Some((count, blob_off))
}

const CDF_FILE_LEN: usize = 32 + 256 * 4;

fn write_cache(path: &Path, bytes: &[u8], cdf: &[u32; 256]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;

    if bytes.len() >= HEADER_LEN {
        let cdf_path = path.with_extension("cdf");
        let cdf_tmp = path.with_extension(format!("cdf-tmp-{}", std::process::id()));
        let mut buf = [0u8; CDF_FILE_LEN];
        // 32B 头部 = magic(4B: "YCDF") + mtime_ns(4B) + count(8B) + size(8B) + mtime_s(8B)
        buf[0..4].copy_from_slice(b"YCDF");
        buf[4..8].copy_from_slice(&bytes[40..44]);
        buf[8..32].copy_from_slice(&bytes[16..40]);
        for (i, &val) in cdf.iter().enumerate() {
            let at = 32 + i * 4;
            buf[at..at + 4].copy_from_slice(&val.to_le_bytes());
        }
        if let Ok(mut file) = File::create(&cdf_tmp) {
            if file.write_all(&buf).is_ok() {
                let _ = fs::rename(&cdf_tmp, &cdf_path);
            }
            let _ = fs::remove_file(&cdf_tmp);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_cdf_sidecar(
    cache_path: &Path,
    meta: &SourceMeta,
    expected_count: usize,
    limit: usize,
) -> Option<usize> {
    use std::os::unix::ffi::OsStrExt;
    let cdf_path = cache_path.with_extension("cdf");
    let bytes = cdf_path.as_os_str().as_bytes();
    if bytes.len() >= 4096 || bytes.contains(&0) {
        return None;
    }
    let mut c_path = [0u8; 4096];
    c_path[..bytes.len()].copy_from_slice(bytes);
    c_path[bytes.len()] = 0;

    // 一次 read 尝试读 1057 字节：返回值恰好等于 1056 (CDF_FILE_LEN) 即同时证明文件未被截断也未超长，省掉一次 lseek。
    let mut buf = [0u8; CDF_FILE_LEN + 1];
    // SAFETY: `c_path` 以 `\0` 结尾，只读打开后一次 `read` 并立即 `close`。
    unsafe {
        let fd = libc::open(c_path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let n = libc::read(fd, buf.as_mut_ptr().cast(), CDF_FILE_LEN + 1);
        libc::close(fd);
        if n != CDF_FILE_LEN as isize {
            return None;
        }
    }

    if &buf[0..4] != b"YCDF" {
        return None;
    }
    let mtime_ns = u32::from_le_bytes(buf[4..8].try_into().ok()?);
    let count = u64::from_le_bytes(buf[8..16].try_into().ok()?) as usize;
    let size = u64::from_le_bytes(buf[16..24].try_into().ok()?);
    let mtime_s = i64::from_le_bytes(buf[24..32].try_into().ok()?);
    if count != expected_count
        || size != meta.size
        || mtime_s != meta.mtime_s
        || mtime_ns != meta.mtime_ns
    {
        return None;
    }

    let at = 32 + limit * 4;
    let fits = u32::from_le_bytes(buf[at..at + 4].try_into().ok()?) as usize;
    if fits > count {
        return None;
    }
    Some(fits)
}

fn cache_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(CACHE_DIR_ENV) {
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        let mut p = PathBuf::from(xdg);
        p.push("yyts");
        return Some(p);
    }
    crate::home_dir().map(|mut home| {
        home.push(".cache/yyts");
        home
    })
}

fn cache_path(source: &Path) -> Option<PathBuf> {
    let mut base = cache_root()?;

    let mut hash = FNV_OFFSET;
    #[cfg(unix)]
    let source_bytes = {
        use std::os::unix::ffi::OsStrExt;
        source.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let lossy = source.to_string_lossy();
    #[cfg(not(unix))]
    let source_bytes = lossy.as_bytes();

    for &byte in source_bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut name = [0u8; 20];
    for (i, slot) in name.iter_mut().enumerate().take(16) {
        let nibble = ((hash >> ((15 - i) * 4)) & 0xF) as usize;
        *slot = HEX[nibble];
    }
    name[16..20].copy_from_slice(b".bin");
    // SAFETY: `name` 仅含 ASCII 十六进制字符和 `.bin`。
    let file_name = unsafe { std::str::from_utf8_unchecked(&name) };
    base.push(file_name);
    Some(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn sample() -> &'static [u8] {
        r#"[
        {"front":"前半句，","behind":"后半句。","type":"a","from":"来源一","length":7},
        {"front":"另一句，","behind":"另一个结尾。","type":"b","from":"来源二","length":8}
    ]"#
        .as_bytes()
    }

    fn all(corpus: &Corpus) -> Vec<(String, String, String)> {
        let mut values: Vec<_> = (0..corpus.len())
            .map(|idx| corpus.sentence_at(idx).unwrap())
            .collect();
        values.sort();
        values
    }

    #[test]
    fn parse_and_read() {
        let corpus = Corpus::parse(sample()).unwrap();
        assert_eq!(corpus.len(), 2);
        assert!(!corpus.is_empty());
        assert_eq!(
            all(&corpus),
            vec![
                ("前半句，".to_owned(), "后半句。".to_owned(), "来源一".to_owned()),
                ("另一句，".to_owned(), "另一个结尾。".to_owned(), "来源二".to_owned()),
            ]
        );
        assert_eq!(corpus.sentence_at(2), None);

        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let idx = corpus.pick_index(&mut rng).unwrap();
        let (front, behind, from) = corpus.sentence_at(idx).unwrap();
        let rendered = corpus.pick_and_format_with_rng(&mut rng).unwrap();
        assert!(rendered.contains(&front));
        assert!(rendered.contains(&behind));
        assert!(rendered.contains(&from));
    }

    #[test]
    fn width_filtered_pick() {
        let json = r#"[
            {"front":"a","behind":"b","type":"t","from":"f","length":2},
            {"front":"中中中中","behind":"中中","type":"t","from":"出处","length":6}
        ]"#;
        let corpus = Corpus::parse(json.as_bytes()).unwrap();
        let narrow = corpus.max_width_at(0).unwrap();
        let wide = corpus.max_width_at(1).unwrap();
        assert!(narrow < wide, "narrow {narrow} wide {wide}");

        let mut rng = rand::rngs::StdRng::seed_from_u64(9);
        for _ in 0..200 {
            let out = corpus.pick_fitting_with_rng(&mut rng, Some(narrow)).unwrap();
            assert!(!out.contains("中中中中"), "超宽的句子不应被选中");
        }

        let out = corpus.pick_fitting_with_rng(&mut rng, Some(1)).unwrap();
        assert!(out.contains('a'));

        for seed in 0..200u64 {
            let idx = corpus.pick_index_fitting(seed, Some(narrow)).unwrap();
            assert!(corpus.max_width_at(idx).unwrap() <= narrow);
            assert!(corpus.pick_index_fitting(seed, None).is_some());
        }
    }

    #[test]
    fn empty_corpus_picks_nothing() {
        let corpus = Corpus::parse(b"[]").unwrap();
        assert_eq!(corpus.len(), 0);
        assert!(corpus.is_empty());
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        assert!(corpus.pick_index(&mut rng).is_none());
        assert!(corpus.pick_fitting_with_rng(&mut rng, Some(80)).is_none());
        assert!(corpus.pick_index_fitting(7, Some(80)).is_none());
        assert!(corpus.format_at(0).is_none());
    }

    #[test]
    fn large_corpus_matches_serde() {
        // 构造足够大的数组触发并行宽度计算路径，再与 serde 顺序解析结果逐项对比。
        let mut json = String::from("[");
        for index in 0..3000 {
            if index > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                r#"{{"front":"前半句{index}，","behind":"后半句{index}。","type":"a","from":"来源{index}","length":10}}"#
            ));
        }
        json.push(']');
        assert!(json.len() > 64 * 1024, "样本太小: {}", json.len());

        let corpus = Corpus::parse(json.as_bytes()).unwrap();
        let parsed: Vec<crate::Sentence> = serde_json::from_str(&json).unwrap();
        let mut want: Vec<_> = parsed
            .into_iter()
            .map(|sentence| (sentence.front, sentence.behind, sentence.from))
            .collect();
        want.sort();
        assert_eq!(all(&corpus), want);
    }

    #[test]
    fn cache_roundtrip_and_invalidation() {
        let dir = std::env::temp_dir().join(format!("yyts-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let cache_dir = dir.join("cache");
        let source = dir.join("sentences.json");
        // SAFETY: 测试进程内没有其他线程依赖该环境变量。
        unsafe { std::env::set_var(CACHE_DIR_ENV, &cache_dir) };

        fs::write(&source, sample()).unwrap();
        let first = Corpus::load(&source).unwrap();
        assert_eq!(first.len(), 2);

        let cache = cache_path(&source).unwrap();
        assert!(cache.exists(), "缓存应当被写入 {}", cache.display());
        let cdf_file = cache.with_extension("cdf");
        assert!(cdf_file.exists(), "CDF 边车应当被写入 {}", cdf_file.display());

        let second = Corpus::load(&source).unwrap();
        assert_eq!(all(&second), all(&first));
        for idx in 0..second.len() {
            assert_eq!(second.format_at(idx), first.format_at(idx));
            let mut buf = [0u8; 4096];
            let written = second.format_into(idx, &mut buf).unwrap();
            assert_eq!(&buf[..written], first.format_at(idx).unwrap().as_bytes());
        }
        // 验证带宽度过滤走边车文件与删除边车文件回退二分均正常。
        for width in [1usize, 20, 40, 80, 300] {
            let idx = second.pick_index_fitting(42, Some(width)).unwrap();
            assert!(second.format_at(idx).is_some());
        }
        let _ = fs::remove_file(&cdf_file);
        for width in [1usize, 20, 40, 80, 300] {
            let idx = second.pick_index_fitting(42, Some(width)).unwrap();
            assert!(second.format_at(idx).is_some());
        }

        // 1) 完全损坏的垃圾缓存 -> 必须回退重建
        fs::write(&cache, b"garbage").unwrap();
        let recovered = Corpus::load(&source).unwrap();
        assert_eq!(recovered.len(), 2);

        // 2) 截断到只剩 64 B 合法头部（丢失记录与文本池） -> 必须检测到长度不符并重建，不能报错
        let full_cache = fs::read(&cache).unwrap();
        assert!(full_cache.len() > HEADER_LEN);
        fs::write(&cache, &full_cache[..HEADER_LEN]).unwrap();
        let recovered_head_only = Corpus::load(&source).unwrap();
        assert_eq!(recovered_head_only.len(), 2);
        let picked_idx = recovered_head_only.pick_index_fitting(7, Some(80)).unwrap();
        assert!(recovered_head_only.format_at(picked_idx).is_some());
        assert_eq!(fs::read(&cache).unwrap().len(), full_cache.len());

        // 3) 截断到一半长度 -> 必须检测到长度不符并重建并修复磁盘缓存
        fs::write(&cache, &full_cache[..full_cache.len() / 2]).unwrap();
        let recovered_half = Corpus::load(&source).unwrap();
        assert_eq!(recovered_half.len(), 2);
        assert_eq!(all(&recovered_half), all(&first));
        assert_eq!(fs::read(&cache).unwrap().len(), full_cache.len());

        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(
            &source,
            r#"[{"front":"新的，","behind":"句子。","type":"c","from":"来源三","length":5}]"#
                .as_bytes(),
        )
        .unwrap();
        let third = Corpus::load(&source).unwrap();
        assert_eq!(third.len(), 1);
        assert_eq!(
            third.sentence_at(0).unwrap(),
            ("新的，".to_owned(), "句子。".to_owned(), "来源三".to_owned())
        );

        // SAFETY: 同上，测试独占该变量。
        unsafe { std::env::remove_var(CACHE_DIR_ENV) };
        let _ = fs::remove_dir_all(&dir);
    }
}
