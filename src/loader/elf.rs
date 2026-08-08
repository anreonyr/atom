// loader/elf.rs — ELF64 头/程序头解析（原子层）
//
// 只做字节读取与格式校验，不接触内存分配/调度（唯一依赖是更原子的
// `memory::addr::VirtAddr`，用于用户半区判定）。装载语义在 load.rs。
//
// 全部字段经 `from_le_bytes` 逐字节读——blob 可能非对齐，`repr(C)` 结构体
// 直接 cast 是 UB。校验在 `parse` 内全量完成，调用方（load.rs）只信不验。

use crate::memory::addr::VirtAddr;

// ── ELF64 常量 ─────────────────────────────────────────────

/// e_ident 偏移
const EI_MAG0: usize = 0; // magic [0x7f, 'E', 'L', 'F']
const EI_CLASS: usize = 4; // ELFCLASS64 = 2
const EI_DATA: usize = 5; // ELFDATA2LSB = 1
const EI_VERSION: usize = 6;

const ELFMAG: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2; // 64 位
const ELFDATA2LSB: u8 = 1; // 小端
const ET_EXEC: u16 = 2; // 静态可执行（非 PIE）
const EM_RISCV: u16 = 243;
const PN_XNUM: u16 = 0xFFFF; // e_phnum 溢出到节 0（不支持）

/// ehdr 字段偏移
const OFF_E_TYPE: usize = 16; // u16
const OFF_E_MACHINE: usize = 18; // u16
const OFF_E_VERSION: usize = 20; // u32
const OFF_E_ENTRY: usize = 24; // u64
const OFF_E_PHOFF: usize = 32; // u64
const OFF_E_EHSIZE: usize = 52; // u16
const OFF_E_PHENTSIZE: usize = 54; // u16
const OFF_E_PHNUM: usize = 56; // u16

/// phdr 字段偏移（大小 56）
const OFF_P_TYPE: usize = 0; // u32
const OFF_P_FLAGS: usize = 4; // u32
const OFF_P_OFFSET: usize = 8; // u64
const OFF_P_VADDR: usize = 16; // u64
const OFF_P_FILESZ: usize = 32; // u64
const OFF_P_MEMSZ: usize = 40; // u64

const PHDR_SIZE: usize = 56;

/// 可装载段类型号（其余类型跳过）。
pub const PT_LOAD: u32 = 1;
/// 段权限位（p_flags）。
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
#[allow(dead_code)] // RISC-V ELF 权限位三元组完整性；loader 恒置 R 位故未单独引用
pub const PF_R: u32 = 4;

// ── 解析结果 ───────────────────────────────────────────────

/// ELF64 可执行文件解析结果 — 持有 blob 生命周期，按需读取程序头。
pub struct Elf64<'a> {
    blob: &'a [u8],
    /// e_entry 入口虚拟地址（已校验：用户半区 + 落在某 PT_LOAD 内）
    entry: usize,
    /// e_phoff 程序头表偏移
    phoff: usize,
    /// e_phnum 程序头数量
    phnum: usize,
}

/// ELF64 程序头（PT_LOAD 视图）— `parse` 过滤后仅含可装载段。
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    /// p_flags 权限位（PF_X / PF_W / PF_R）
    pub flags: u32,
    /// p_offset 段在文件内的偏移
    pub offset: usize,
    /// p_vaddr 装载虚拟地址
    pub vaddr: usize,
    /// p_filesz 文件内大小
    pub filesz: usize,
    /// p_memsz 内存内大小（> filesz 时尾部填零，即 .bss）
    pub memsz: usize,
}

/// ELF 解析错误 — 变体名即行为（格式非法 / 不支持 / 越界 / 非用户地址）。
#[derive(Debug)]
#[allow(dead_code)] // payload 为错误语义上下文，仅在 Debug 输出时读取（dead_code 不识别）
pub enum ElfError {
    /// 长度不足或越界（字段读不到 / 程序头表越界）
    Truncated(&'static str),
    /// 不是 ELF64 小端可执行（magic / class / data / version 不符）
    BadFormat(&'static str),
    /// 合法但不支持（ET_DYN / ET_REL、非 RISC-V、PN_XNUM）
    Unsupported(&'static str),
    /// 地址不在用户半区（e_entry / PT_LOAD p_vaddr）
    NotUserAddress(&'static str),
    /// 段语义非法（memsz < filesz / 无 PT_LOAD / entry 不在段内）
    InvalidSegment(&'static str),
}

// ── 逐字节读取 ─────────────────────────────────────────────

/// 读 `blob[off..off+2]` 为小端 u16。
fn u16_at(blob: &[u8], off: usize) -> Option<u16> {
    let b = blob.get(off..off + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// 读 `blob[off..off+4]` 为小端 u32。
fn u32_at(blob: &[u8], off: usize) -> Option<u32> {
    let b = blob.get(off..off + 4)?;
    Some(u32::from_le_bytes(b.try_into().ok()?))
}

/// 读 `blob[off..off+8]` 为小端 u64。
fn u64_at(blob: &[u8], off: usize) -> Option<u64> {
    let b = blob.get(off..off + 8)?;
    Some(u64::from_le_bytes(b.try_into().ok()?))
}

/// 原始程序头（未过滤类型），`parse` 与 `phdr` 共用。
#[derive(Clone, Copy)]
struct RawPhdr {
    p_type: u32,
    flags: u32,
    offset: usize,
    vaddr: usize,
    filesz: usize,
    memsz: usize,
}

/// 从 `off` 读一个原始程序头；越界返回 `None`。
fn raw_phdr(blob: &[u8], off: usize) -> Option<RawPhdr> {
    Some(RawPhdr {
        p_type: u32_at(blob, off + OFF_P_TYPE)?,
        flags: u32_at(blob, off + OFF_P_FLAGS)?,
        offset: u64_at(blob, off + OFF_P_OFFSET)? as usize,
        vaddr: u64_at(blob, off + OFF_P_VADDR)? as usize,
        filesz: u64_at(blob, off + OFF_P_FILESZ)? as usize,
        memsz: u64_at(blob, off + OFF_P_MEMSZ)? as usize,
    })
}

// ── 解析与校验 ─────────────────────────────────────────────

impl<'a> Elf64<'a> {
    /// 解析并校验 ELF64 可执行文件。
    ///
    /// 校验清单：magic/class/endian/version、e_type(ET_EXEC)/e_machine(RISC-V)、
    /// ehdr/phdr 大小、程序头表边界；逐 PT_LOAD 校验 memsz>=filesz、文件区间
    /// 在 blob 内、vaddr 在用户半区、vaddr+memsz 不溢出；至少一个 PT_LOAD；
    /// entry 在用户半区且落在某 PT_LOAD 内（防首条取指缺页）。
    ///
    /// # Errors
    ///
    /// 任一校验失败返回对应的 [`ElfError`]（见变体注释）。
    pub fn parse(blob: &'a [u8]) -> Result<Self, ElfError> {
        if blob.len() < 64 {
            return Err(ElfError::Truncated("ehdr"));
        }
        // e_ident
        if blob.get(EI_MAG0..EI_MAG0 + 4) != Some(ELFMAG.as_slice()) {
            return Err(ElfError::BadFormat("magic"));
        }
        if blob[EI_CLASS] != ELFCLASS64 {
            return Err(ElfError::BadFormat("class"));
        }
        if blob[EI_DATA] != ELFDATA2LSB {
            return Err(ElfError::BadFormat("endianness"));
        }
        if blob[EI_VERSION] != 1 {
            return Err(ElfError::BadFormat("ident version"));
        }
        // 已保证 len >= 64，以下偏移均在界内，unwrap 安全
        if u16_at(blob, OFF_E_TYPE) != Some(ET_EXEC) {
            return Err(ElfError::Unsupported("e_type (expect ET_EXEC)"));
        }
        if u16_at(blob, OFF_E_MACHINE) != Some(EM_RISCV) {
            return Err(ElfError::Unsupported("e_machine (expect RISC-V)"));
        }
        if u32_at(blob, OFF_E_VERSION) != Some(1) {
            return Err(ElfError::BadFormat("e_version"));
        }
        if u16_at(blob, OFF_E_EHSIZE) != Some(64) {
            return Err(ElfError::BadFormat("e_ehsize"));
        }
        if u16_at(blob, OFF_E_PHENTSIZE) != Some(PHDR_SIZE as u16) {
            return Err(ElfError::BadFormat("e_phentsize"));
        }
        let phnum = u16_at(blob, OFF_E_PHNUM).unwrap() as usize;
        if phnum == 0 {
            return Err(ElfError::InvalidSegment("e_phnum == 0"));
        }
        if phnum as u16 == PN_XNUM {
            return Err(ElfError::Unsupported("e_phnum == PN_XNUM"));
        }
        let phoff = u64_at(blob, OFF_E_PHOFF).unwrap() as usize;
        // checked：程序头表整体在 blob 内（防 e_phoff/e_phnum 伪造越界）
        let table_end = phoff
            .checked_add(phnum.checked_mul(PHDR_SIZE).ok_or(ElfError::Truncated("phdr table"))?)
            .ok_or(ElfError::Truncated("phdr table"))?;
        if table_end > blob.len() {
            return Err(ElfError::Truncated("phdr table"));
        }
        let entry = u64_at(blob, OFF_E_ENTRY).unwrap() as usize;

        // 逐 PT_LOAD 校验 + 记录 entry 归属
        let mut has_load = false;
        let mut entry_in_segment = false;
        for i in 0..phnum {
            let ph = raw_phdr(blob, phoff + i * PHDR_SIZE).unwrap();
            if ph.p_type != PT_LOAD {
                continue;
            }
            has_load = true;
            if ph.memsz < ph.filesz {
                return Err(ElfError::InvalidSegment("p_memsz < p_filesz"));
            }
            let file_end = ph
                .offset
                .checked_add(ph.filesz)
                .ok_or(ElfError::InvalidSegment("p_offset + p_filesz overflow"))?;
            if file_end > blob.len() {
                return Err(ElfError::Truncated("PT_LOAD file range"));
            }
            let seg_end = ph
                .vaddr
                .checked_add(ph.memsz)
                .ok_or(ElfError::InvalidSegment("p_vaddr + p_memsz overflow"))?;
            if !VirtAddr::from_raw(ph.vaddr).is_user() {
                return Err(ElfError::NotUserAddress("PT_LOAD vaddr"));
            }
            if entry >= ph.vaddr && entry < seg_end {
                entry_in_segment = true;
            }
        }
        if !has_load {
            return Err(ElfError::InvalidSegment("no PT_LOAD"));
        }
        if !VirtAddr::from_raw(entry).is_user() {
            return Err(ElfError::NotUserAddress("entry"));
        }
        if !entry_in_segment {
            return Err(ElfError::InvalidSegment("entry not in any PT_LOAD"));
        }
        Ok(Self {
            blob,
            entry,
            phoff,
            phnum,
        })
    }

    /// 入口虚拟地址（用户空间）。
    pub fn entry(&self) -> usize {
        self.entry
    }

    /// 程序头数量。
    pub fn phnum(&self) -> usize {
        self.phnum
    }

    /// 读取第 `index` 个程序头；非 PT_LOAD 或越界返回 `None`。
    ///
    /// 由 `parse` 校验过：`index` 合法时表区间必在界内。
    pub fn phdr(&self, index: usize) -> Option<Segment> {
        let off = self
            .phoff
            .checked_add(index.checked_mul(PHDR_SIZE)?)?
            .checked_add(PHDR_SIZE)?;
        if off > self.blob.len() {
            return None;
        }
        let ph = raw_phdr(self.blob, self.phoff + index * PHDR_SIZE)?;
        (ph.p_type == PT_LOAD).then_some(Segment {
            flags: ph.flags,
            offset: ph.offset,
            vaddr: ph.vaddr,
            filesz: ph.filesz,
            memsz: ph.memsz,
        })
    }
}
