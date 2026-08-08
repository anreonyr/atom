// file/registry — 设备注册表（复用器核心）
//
// 多路复用器模型：file 域不反向依赖任何提供方。各设备域（io 标准流、
// uart 服务、log、devfs 内建）在启动时把 `&'static dyn File` 实例注册到
// 本表，devfs 枚举注册表构建 /dev 节点——所有条目受顶层契约 `ops::File`
// 约束（Linux register_chrdev + cdev 的对应物）。
//
// 注册时机：boot 早期（allocator 就绪后、create_devfs 前）；查询为快照
// 拷贝（脱离锁生命周期，devfs 构建时使用）。

use alloc::vec::Vec;

use crate::file::ops::File;
use crate::lock::SpinLock;

/// 注册表条目 — 设备路径 + File 实现。
#[derive(Clone)]
pub struct RegistryEntry {
    /// 设备路径（如 "/dev/stdin"）— 注册表内唯一
    pub name: &'static str,
    /// File 能力实现（提供方所有，仅受顶层契约约束）
    pub file: &'static dyn File,
}

/// 注册错误
#[derive(Debug)]
#[allow(dead_code)] // NameTaken 载荷当前无读取方（register 调用方丢弃错误），保留诊断信息
pub enum RegistryError {
    /// 注册时名字已存在
    NameTaken(&'static str),
}

/// 设备注册表 — SpinLock 保护，条目只增不删。
static REGISTRY: SpinLock<Vec<RegistryEntry>> = SpinLock::new(Vec::new());

/// 注册设备 — 名字冲突返回 [`RegistryError::NameTaken`]。
pub fn register(name: &'static str, file: &'static dyn File) -> Result<(), RegistryError> {
    let mut list = REGISTRY.lock();
    if list.iter().any(|e| e.name == name) {
        return Err(RegistryError::NameTaken(name));
    }
    list.push(RegistryEntry { name, file });
    Ok(())
}

/// 枚举全部已注册设备 — 快照拷贝，调用方脱离锁使用。
pub fn all() -> Vec<RegistryEntry> {
    REGISTRY.lock().clone()
}
