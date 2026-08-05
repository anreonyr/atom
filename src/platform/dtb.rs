// DTB 类型 — 零分配 FDT 解析器
//
// 提供 Dtb 句柄、Node 节点、Walk 遍历器三层抽象。

use core::ptr::NonNull;

use super::cell;
use super::header::FdtHeader;

// ── Token 常量 ──────────────────────────────────────────────

pub(crate) const TOKEN_BEGIN_NODE: u32 = 0x0000_0001;
pub(crate) const TOKEN_END_NODE: u32 = 0x0000_0002;
pub(crate) const TOKEN_PROP: u32 = 0x0000_0003;
pub(crate) const TOKEN_END: u32 = 0x0000_0009;

// ── Dtb ─────────────────────────────────────────────────────

/// 校验过的 FDT 句柄。
#[derive(Copy, Clone)]
pub struct Dtb {
    base: NonNull<u8>,
    struct_off: u32,
    struct_size: u32,
    strings_off: u32,
    strings_size: u32,
}

unsafe impl Send for Dtb {}
unsafe impl Sync for Dtb {}

impl Dtb {
    /// 校验 FDT 头部并创建句柄。
    ///
    /// # Safety
    ///
    /// `ptr` 须指向有效的 FDT 数据。
    pub unsafe fn new(ptr: usize) -> Result<Self, super::header::DtbError> { unsafe {
        let header = FdtHeader::validate(ptr)?;
        Ok(Self {
            base: NonNull::new_unchecked(ptr as *mut u8),
            struct_off: header.off_dt_struct(),
            struct_size: header.size_dt_struct(),
            strings_off: header.off_dt_strings(),
            strings_size: header.size_dt_strings(),
        })
    }}

    /// 深度优先遍历全部非根节点（根节点被跳过）。
    pub fn walk(&self) -> Walk<'_> {
        Walk {
            dtb: self,
            cursor: 0,
            depth: 0,
            cell_stack: [2u32; 16],
            cell_stack_sz: [2u32; 16],
            skip: 0,
        }
    }

    // ── 内部 helpers ──────────────────────────────────────────

    unsafe fn read_u32(&self, off: u32) -> u32 { unsafe {
        let p = self
            .base
            .add((self.struct_off + off) as usize)
            .cast()
            .read_volatile();
        u32::from_be(p)
    }}

    fn in_bounds(&self, off: u32) -> bool {
        off <= self.struct_size && self.struct_size - off >= 4
    }

    /// 跳过节点名（null-terminated + 4 对齐），返回越过名后的偏移。
    unsafe fn skip_name(&self, mut off: u32) -> u32 { unsafe {
        let b = self.base.add(self.struct_off as usize);
        while b.add(off as usize).read() != 0 {
            off += 1;
        }
        (off + 4) & !3
    }}

    /// 读节点名（`node_off` 指向 BEGIN_NODE token）。
    unsafe fn node_name(&self, node_off: u32) -> &str { unsafe {
        let start = self
            .base
            .add(self.struct_off as usize + node_off as usize + 4);
        let mut len = 0usize;
        while start.add(len).read() != 0 {
            len += 1;
        }
        core::str::from_utf8(start.cast_slice(len).as_ref()).unwrap_or("")
    }}

    /// 从字符串块读属性名。
    unsafe fn string_at(&self, name_off: u32) -> &str { unsafe {
        if name_off >= self.strings_size {
            return "";
        }
        let start = self.base.add(self.strings_off as usize + name_off as usize);
        let mut len = 0usize;
        let max = (self.strings_size - name_off) as usize;
        while len < max && start.add(len).read() != 0 {
            len += 1;
        }
        core::str::from_utf8(start.cast_slice(len).as_ref()).unwrap_or("")
    }}
}

// ── Walk ────────────────────────────────────────────────────

/// 深度优先遍历器。
pub struct Walk<'a> {
    dtb: &'a Dtb,
    cursor: u32,
    depth: u8,
    /// 每层的 #address-cells（来自节点自身属性，适用于后代）
    cell_stack: [u32; 16],
    cell_stack_sz: [u32; 16],
    /// >0 时跳过子树（/chosen 用）
    skip: u8,
}

impl<'a> Walk<'a> {
    /// 读取当前节点的 PROP 内容（跳过 value，返回 name→value 的元组）。
    /// 只在 cursor 指向 TOKEN_PROP 时有效（但由 next() 保证）。
    fn consume_prop(&mut self) -> (u32, &'a [u8]) {
        let dtb = self.dtb;
        let len = unsafe { dtb.read_u32(self.cursor) } as usize;
        self.cursor += 4;
        let name_off = unsafe { dtb.read_u32(self.cursor) };
        self.cursor += 4;
        let padded = ((len + 3) & !3) as u32;
        let val = unsafe {
            dtb.base
                .add(dtb.struct_off as usize + self.cursor as usize)
                .cast_slice(len)
                .as_ref()
        };
        self.cursor += padded;
        (name_off, val)
    }
}

impl Iterator for Walk<'_> {
    type Item = Node;

    fn next(&mut self) -> Option<Node> {
        loop {
            if !self.dtb.in_bounds(self.cursor) {
                return None;
            }
            let token = unsafe { self.dtb.read_u32(self.cursor) };
            self.cursor += 4;

            match token {
                TOKEN_BEGIN_NODE => {
                    let node_off = self.cursor - 4;
                    self.cursor = unsafe { self.dtb.skip_name(self.cursor) };
                    self.depth += 1;

                    // 跳过根节点 (offset 0)
                    if node_off == 0 {
                        continue;
                    }

                    // 如果正在跳过子树（已匹配 /chosen 的父节点），继续跳
                    if self.skip > 0 {
                        self.skip += 1;
                        continue;
                    }

                    // 检查 /chosen 子树：跳过
                    let name = unsafe { self.dtb.node_name(node_off) };
                    if name == "chosen" {
                        self.skip = 1;
                        continue;
                    }

                    // 获取当前节点的父 cells（来自 cell_stack[depth-2]）
                    let d = (self.depth - 1) as usize;
                    let (ac, sc) = if d == 0 {
                        (2, 2)
                    } else {
                        (self.cell_stack[d - 1], self.cell_stack_sz[d - 1])
                    };

                    // 初始化本层的 cells（继承父级，稍后被 #address-cells 等覆盖）
                    if d < 16 {
                        self.cell_stack[d] = ac;
                        self.cell_stack_sz[d] = sc;
                    }

                    return Some(Node {
                        offset: node_off,
                        address_cells: ac,
                        size_cells: sc,
                    });
                }

                TOKEN_END_NODE => {
                    if self.skip > 0 {
                        self.skip -= 1;
                    }
                    if self.depth == 0 {
                        return None;
                    }
                    self.depth -= 1;
                }

                TOKEN_PROP => {
                    let (name_off, val) = self.consume_prop();
                    if val.len() == 4 && self.depth > 0 {
                        let name = unsafe { self.dtb.string_at(name_off) };
                        let d = (self.depth - 1) as usize;
                        if d < 16 {
                            let v = u32::from_be_bytes(val.try_into().unwrap());
                            match name {
                                "#address-cells" => self.cell_stack[d] = v,
                                "#size-cells" => self.cell_stack_sz[d] = v,
                                _ => {}
                            }
                        }
                    }
                }

                TOKEN_END => return None,
                _ => {} // NOP
            }
        }
    }
}

// ── Node ────────────────────────────────────────────────────

/// DTB 节点句柄（Copy）。
#[derive(Copy, Clone, Debug)]
pub struct Node {
    offset: u32,
    address_cells: u32,
    size_cells: u32,
}

impl Node {
    /// 节点名称（如 `"uart@1000000"`）。
    pub fn name<'a>(&self, dtb: &'a Dtb) -> &'a str {
        unsafe { dtb.node_name(self.offset) }
    }

    /// reg 的第 `index` 组 `(base, size)`。
    pub fn property_reg(&self, dtb: &Dtb, index: usize) -> Option<(u64, u64)> {
        let raw = dtb.node_property_raw(self.offset, "reg")?;
        let cell_size = (self.address_cells + self.size_cells) as usize * 4;
        let off = index * cell_size;
        if off + cell_size > raw.len() {
            return None;
        }
        let mut pos = off;
        let base = cell::read_cells(raw, &mut pos, self.address_cells)?;
        let size = cell::read_cells(raw, &mut pos, self.size_cells)?;
        Some((base, size))
    }

    /// u32 属性（如 `"interrupts"`）。
    pub fn property_u32(&self, dtb: &Dtb, name: &str) -> Option<u32> {
        let raw = dtb.node_property_raw(self.offset, name)?;
        cell::read_cell_u32(raw, 0)
    }

    /// 字符串属性（如 `"compatible"`）。
    pub fn property_string<'a>(&self, dtb: &'a Dtb, name: &str) -> Option<&'a str> {
        let raw = dtb.node_property_raw(self.offset, name)?;
        let trimmed = raw.split(|&b| b == 0).next().unwrap_or(raw);
        core::str::from_utf8(trimmed).ok()
    }

    /// 原始属性字节。
    #[allow(dead_code)] // 原始属性查询预留
    pub fn property_raw<'a>(&self, dtb: &'a Dtb, name: &str) -> Option<&'a [u8]> {
        dtb.node_property_raw(self.offset, name)
    }
}

// ── Dtb property reader ─────────────────────────────────────

impl Dtb {
    /// 在 node_off 节点内找属性 `name` 的值。
    pub(crate) fn node_property_raw(&self, node_off: u32, name: &str) -> Option<&[u8]> {
        let mut off = node_off + 4; // past BEGIN_NODE
        off = unsafe { self.skip_name(off) };

        loop {
            if !self.in_bounds(off) {
                return None;
            }
            let token = unsafe { self.read_u32(off) };
            off += 4;

            match token {
                TOKEN_PROP => {
                    if !self.in_bounds(off + 7) {
                        return None;
                    }
                    let len = unsafe { self.read_u32(off) } as usize;
                    off += 4;
                    let name_off = unsafe { self.read_u32(off) };
                    off += 4;
                    let prop_name: &str = unsafe { self.string_at(name_off) };

                    if prop_name == name {
                        let val = unsafe {
                            self.base
                                .add(self.struct_off as usize + off as usize)
                                .cast_slice(len)
                                .as_ref()
                        };
                        return Some(val);
                    }

                    off += ((len + 3) & !3) as u32;
                }

                TOKEN_BEGIN_NODE => {
                    off = unsafe { self.skip_name(off) };
                    let mut depth = 1u32;
                    while depth > 0 {
                        if !self.in_bounds(off) {
                            return None;
                        }
                        let t = unsafe { self.read_u32(off) };
                        off += 4;
                        match t {
                            TOKEN_BEGIN_NODE => {
                                off = unsafe { self.skip_name(off) };
                                depth += 1;
                            }
                            TOKEN_END_NODE => depth -= 1,
                            TOKEN_PROP => {
                                let l = unsafe { self.read_u32(off) } as usize;
                                off += 8;
                                off += ((l + 3) & !3) as u32;
                            }
                            TOKEN_END => return None,
                            _ => {}
                        }
                    }
                }

                TOKEN_END_NODE | TOKEN_END => return None,
                _ => {} // NOP
            }
        }
    }
}
