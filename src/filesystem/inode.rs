// Inode — 文件系统节点抽象
//
// Inode 是 VFS 的核心抽象，代表文件系统树中的一个节点。
// 引导期通过 InodeBuilder 创建并以 Box::leak 获得 'static 生命周期，
// 此后不可变——读取路径无锁。

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use crate::filesystem::traits::{FileControl, FileRead, FileSeek, FileWrite};

// ── InodeType ─────────────────────────────────────────────

/// 文件系统节点类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeType {
    /// 目录 — 包含子节点
    Directory,
    /// 字节设备 — 流式传输，无 seek（UART、console）
    ByteDevice,
    /// 块设备 — 按块读写，支持随机访问（磁盘）
    BlockDevice,
    /// 普通文件 — 来自真实文件系统
    Regular,
}

// ── Inode ─────────────────────────────────────────────────

/// 文件系统节点。
///
/// 每个 Inode 代表文件系统树中的一个命名对象。设备驱动通过 trait 对象引用
/// 嵌入节点中——trait 对象在 VFS 层，不在 hub 层。
///
/// Inode 在引导期创建后不可变，所有字段均为共享引用。
pub struct Inode {
    /// 节点名称（如 "console"、"dev"）
    pub name: &'static str,
    /// 节点类型
    pub inode_type: InodeType,
    /// 读操作实现（设备/文件支持时非空）
    pub read: Option<&'static dyn FileRead>,
    /// 写操作实现
    pub write: Option<&'static dyn FileWrite>,
    /// 定位操作实现
    pub seek: Option<&'static dyn FileSeek>,
    /// 设备控制操作实现
    pub control: Option<&'static dyn FileControl>,
    /// 子节点（仅目录类型非空）
    pub children: &'static [&'static Inode],
}

impl fmt::Debug for Inode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inode")
            .field("name", &self.name)
            .field("inode_type", &self.inode_type)
            .field(
                "children",
                &self.children.iter().map(|c| c.name).collect::<Vec<_>>(),
            )
            .finish()
    }
}

// ── InodeBuilder ──────────────────────────────────────────

/// Inode 构造器 — 引导期链式构造。
///
/// # 示例
///
/// ```ignore
/// let console = InodeBuilder::new("console", InodeType::ByteDevice)
///     .with_read(&CONSOLE)
///     .with_write(&CONSOLE)
///     .build();
/// ```
pub struct InodeBuilder {
    name: &'static str,
    inode_type: InodeType,
    read: Option<&'static dyn FileRead>,
    write: Option<&'static dyn FileWrite>,
    seek: Option<&'static dyn FileSeek>,
    control: Option<&'static dyn FileControl>,
    children: Vec<&'static Inode>,
}

impl InodeBuilder {
    /// 创建新的构造器。
    pub fn new(name: &'static str, inode_type: InodeType) -> Self {
        Self {
            name,
            inode_type,
            read: None,
            write: None,
            seek: None,
            control: None,
            children: Vec::new(),
        }
    }

    /// 设置读操作实现。
    pub fn with_read(mut self, r: &'static dyn FileRead) -> Self {
        self.read = Some(r);
        self
    }

    /// 设置写操作实现。
    pub fn with_write(mut self, w: &'static dyn FileWrite) -> Self {
        self.write = Some(w);
        self
    }

    /// 设置定位操作实现。
    pub fn with_seek(mut self, s: &'static dyn FileSeek) -> Self {
        self.seek = Some(s);
        self
    }

    /// 设置设备控制操作实现。
    pub fn with_control(mut self, c: &'static dyn FileControl) -> Self {
        self.control = Some(c);
        self
    }

    /// 添加子节点（仅目录类型使用）。
    pub fn with_child(mut self, child: &'static Inode) -> Self {
        self.children.push(child);
        self
    }

    /// 消费构造器，在堆上分配 Inode 并泄漏获得 'static 引用。
    pub fn build(self) -> &'static Inode {
        let children: &'static [&'static Inode] = if self.children.is_empty() {
            &[]
        } else {
            Vec::leak(self.children)
        };

        Box::leak(Box::new(Inode {
            name: self.name,
            inode_type: self.inode_type,
            read: self.read,
            write: self.write,
            seek: self.seek,
            control: self.control,
            children,
        }))
    }
}

// ── 路径解析 ──────────────────────────────────────────────

/// 从根节点按路径查找目标 Inode。
///
/// 无分配、纯函数——按 '/' 分割路径组件，从根开始逐级在
/// 目录节点的 `children` 列表中线性查找。
///
/// 暂不支持 `..`（无 parent 指针）。
pub fn resolve<'a>(root: &'a Inode, path: &str) -> Option<&'a Inode> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Some(root);
    }

    let mut current = root;
    for part in path.split('/') {
        // 跳过空组件（连续斜杠）和当前目录
        if part.is_empty() || part == "." {
            continue;
        }

        // 查找匹配名称的子节点
        current = current.children.iter().find(|c| c.name == part)?;
    }

    Some(current)
}
