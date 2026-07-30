// FDT 字符串块安全解析
//
// 封装字符串块裸指针，提供边界检查的字符串查找。

use super::header::FdtHeader;
use core::str;

/// 字符串块解析器。
///
/// 持有字符串块基址和大小，提供带边界检查的 null-terminated 字符串查找。
pub(crate) struct StringTable {
    base: *const u8,
    size: u32,
}

impl StringTable {
    /// 从已验证的 FDT 头部创建字符串块解析器。
    ///
    /// # Safety
    ///
    /// `header` 必须是经过 `FdtHeader::validate()` 验证的有效头部。
    pub(crate) unsafe fn new(header: &FdtHeader) -> Self {
        let dtb_base = header as *const _ as usize;
        Self {
            base: (dtb_base + header.off_dt_strings() as usize) as *const u8,
            size: header.size_dt_strings(),
        }
    }

    /// 在字符串块中查找偏移 `offset` 处的 null-terminated 字符串。
    ///
    /// 返回 `None` 若：
    /// - `offset` 超出字符串块大小
    /// - 在字符串块范围内未找到 null 终止符
    /// - 字符串不是有效 UTF-8
    pub(crate) fn get(&self, offset: u32) -> Option<&'static str> {
        if offset >= self.size {
            return None;
        }
        // SAFETY: offset 已验证在 [0, size) 范围内
        let ptr = unsafe { self.base.add(offset as usize) };
        let remaining = (self.size - offset) as usize;
        let len = find_null(ptr, remaining)?;
        // SAFETY: ptr..ptr+len 在字符串块范围内，且以 null 终止
        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        str::from_utf8(slice).ok()
    }
}

/// 在 `max_len` 范围内扫描 null 字节，返回 null 之前的字节数。
///
/// 若在 `max_len` 字节内未找到 null，返回 `None`。
fn find_null(ptr: *const u8, max_len: usize) -> Option<usize> {
    let mut len = 0usize;
    while len < max_len {
        // SAFETY: len < max_len，ptr + len 在合法范围内
        if unsafe { ptr.add(len).read_volatile() } == 0 {
            return Some(len);
        }
        len += 1;
    }
    None
}
