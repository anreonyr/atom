// FDT (Flattened Device Tree) 头部解析
//
// 解析 DTB 的 40 字节头部，验证魔数，提取结构块和字符串块的偏移与大小。
// 所有字段均为大端序 u32。

use core::{fmt, ptr::NonNull};

/// FDT 魔数（big-endian: 0xD00DFEED）。
pub const FDT_MAGIC: u32 = 0xD00D_FEED;

/// FDT 支持的最低版本 (v16)。
const MIN_VERSION: u32 = 16;

/// 头部大小（固定 40 字节）。
const HEADER_SIZE: u32 = 40;

/// DTB 头部验证错误。
#[derive(Debug)]
pub enum DtbError {
    /// 无效的魔数
    BadMagic(u32),
    /// 版本低于最低要求
    BadVersion(u32),
    /// 块偏移超出 DTB 总大小
    OutOfBounds {
        field: &'static str,
        offset: u32,
        size: u32,
        totalsize: u32,
    },
}

impl fmt::Display for DtbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DtbError::BadMagic(m) => write!(f, "bad FDT magic: {:#010x}", m),
            DtbError::BadVersion(v) => {
                write!(f, "unsupported FDT version: {} (min {})", v, MIN_VERSION)
            }
            DtbError::OutOfBounds {
                field,
                offset,
                size,
                totalsize,
            } => {
                write!(
                    f,
                    "FDT {} block out of bounds: offset={:#x} size={:#x} totalsize={:#x}",
                    field, offset, size, totalsize
                )
            }
        }
    }
}

/// FDT 头部 (v17)，所有字段存储为原始大端序 u32。
///
/// 共 10 个 u32 字段，40 字节。
#[repr(C)]
pub struct FdtHeader {
    magic: u32,             // 0x00
    totalsize: u32,         // 0x04
    off_dt_struct: u32,     // 0x08
    off_dt_strings: u32,    // 0x0C
    off_mem_rsvmap: u32,    // 0x10
    version: u32,           // 0x14
    last_comp_version: u32, // 0x18
    boot_cpuid_phys: u32,   // 0x1C
    size_dt_strings: u32,   // 0x20
    size_dt_struct: u32,    // 0x24
}

impl FdtHeader {
    /// 验证 DTB 头部：检查魔数、版本、以及各块的边界。
    ///
    /// 返回指向 DTB 内存中头部的静态引用。
    ///
    /// # Safety
    ///
    /// `dtb_ptr` 必须指向有效的 FDT 头部（至少 `totalsize` 字节可读）。
    pub unsafe fn validate(dtb_ptr: usize) -> Result<&'static Self, DtbError> {
        unsafe {
            let header = NonNull::new_unchecked(dtb_ptr as *mut Self).as_ref();

            let magic = u32::from_be(header.magic);
            if magic != FDT_MAGIC {
                return Err(DtbError::BadMagic(magic));
            }

            let version = u32::from_be(header.version);
            if version < MIN_VERSION {
                return Err(DtbError::BadVersion(version));
            }

            let totalsize = u32::from_be(header.totalsize);

            // 验证内存保留映射块（至少需覆盖头部，即 40 字节）
            let mem_rsvmap = u32::from_be(header.off_mem_rsvmap);
            if mem_rsvmap < HEADER_SIZE || mem_rsvmap > totalsize {
                return Err(DtbError::OutOfBounds {
                    field: "mem_rsvmap",
                    offset: mem_rsvmap,
                    size: 0,
                    totalsize,
                });
            }

            // 验证结构块在 DTB 范围内
            let off_struct = u32::from_be(header.off_dt_struct);
            let size_struct = u32::from_be(header.size_dt_struct);
            let struct_end = off_struct
                .checked_add(size_struct)
                .ok_or(DtbError::OutOfBounds {
                    field: "dt_struct",
                    offset: off_struct,
                    size: size_struct,
                    totalsize,
                })?;
            if struct_end > totalsize {
                return Err(DtbError::OutOfBounds {
                    field: "dt_struct",
                    offset: off_struct,
                    size: size_struct,
                    totalsize,
                });
            }

            // 验证字符串块在 DTB 范围内
            let off_strings = u32::from_be(header.off_dt_strings);
            let size_strings = u32::from_be(header.size_dt_strings);
            let strings_end =
                off_strings
                    .checked_add(size_strings)
                    .ok_or(DtbError::OutOfBounds {
                        field: "dt_strings",
                        offset: off_strings,
                        size: size_strings,
                        totalsize,
                    })?;
            if strings_end > totalsize {
                return Err(DtbError::OutOfBounds {
                    field: "dt_strings",
                    offset: off_strings,
                    size: size_strings,
                    totalsize,
                });
            }

            Ok(header)
        }
    }

    /// DTB 总大小 (bytes，大端序解码)。
    #[inline]
    #[allow(dead_code)]
    pub fn totalsize(&self) -> u32 {
        u32::from_be(self.totalsize)
    }

    /// 结构块偏移 (bytes，大端序解码)。
    #[inline]
    pub fn off_dt_struct(&self) -> u32 {
        u32::from_be(self.off_dt_struct)
    }

    /// 字符串块偏移 (bytes，大端序解码)。
    #[inline]
    pub fn off_dt_strings(&self) -> u32 {
        u32::from_be(self.off_dt_strings)
    }

    /// 结构块大小 (bytes，大端序解码)。
    #[inline]
    pub fn size_dt_struct(&self) -> u32 {
        u32::from_be(self.size_dt_struct)
    }

    /// 字符串块大小 (bytes，大端序解码)。
    #[inline]
    pub fn size_dt_strings(&self) -> u32 {
        u32::from_be(self.size_dt_strings)
    }

    /// 引导 CPU 物理 ID（大端序解码）。
    #[inline]
    #[allow(dead_code)]
    pub fn boot_cpuid_phys(&self) -> u32 {
        u32::from_be(self.boot_cpuid_phys)
    }

    /// 返回内存保留映射块的 (偏移, 条目数)。
    ///
    /// 保留映射块由 `(address: u64, size: u64)` 对组成，
    /// 以 `(0, 0)` 对终止。返回值可用于后续遍历。
    #[inline]
    #[allow(dead_code)]
    pub fn mem_rsvmap(&self) -> (u32, u32) {
        let off = u32::from_be(self.off_mem_rsvmap);
        let struct_off = u32::from_be(self.off_dt_struct);
        // 保留映射块从 off_mem_rsvmap 到 off_dt_struct（结构块之前）
        let size = struct_off.saturating_sub(off);
        (off, size)
    }
}

impl fmt::Debug for FdtHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FdtHeader")
            .field("magic", &format_args!("{:#010x}", u32::from_be(self.magic)))
            .field("totalsize", &u32::from_be(self.totalsize))
            .field("version", &u32::from_be(self.version))
            .field("boot_cpuid", &u32::from_be(self.boot_cpuid_phys))
            .finish()
    }
}
