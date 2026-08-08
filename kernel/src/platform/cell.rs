// DTB cell 读取工具函数
//
// FDT 使用 32-bit big-endian "cells" 存储数值。
// 本模块提供解析这些 cell 的零分配工具。

/// 从属性值中读取一个 32-bit cell（大端序）。
///
/// `offset` 是属性值字节数组中的起始偏移。
/// 若剩余数据不足 4 字节则返回 `None`。
pub fn read_cell_u32(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    if data.len() < end {
        return None;
    }
    let bytes: [u8; 4] = data[offset..end].try_into().ok()?;
    Some(u32::from_be_bytes(bytes))
}

/// 从属性值中按 `cells` 个大端序 u32 读取 u64。
///
/// cells=0 → Some(0)
/// cells=1 → 低 32 位有效
/// cells=2 → hi<<32 | lo
/// cells>2 → None（不支持）
///
/// `offset` 为可变引用，读取成功后会前进相应的字节数。
pub fn read_cells(data: &[u8], offset: &mut usize, cells: u32) -> Option<u64> {
    match cells {
        0 => Some(0),
        1 => {
            let v = read_cell_u32(data, *offset)? as u64;
            *offset += 4;
            Some(v)
        }
        2 => {
            let hi = read_cell_u32(data, *offset)? as u64;
            *offset += 4;
            let lo = read_cell_u32(data, *offset)? as u64;
            *offset += 4;
            Some((hi << 32) | lo)
        }
        _ => None,
    }
}
