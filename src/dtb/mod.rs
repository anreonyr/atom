// DTB (Device Tree Blob) 解析器
//
// 解析 Flattened Device Tree (FDT) 格式，用于从固件获取硬件配置。
// 零分配设计：解析器不依赖 global allocator，仅使用栈变量。
//
// # 模块
//
// | 模块       | 职责                                  |
// |-----------|--------------------------------------|
// | `header`  | FDT 头部校验 (magic/version/bounds)   |
// | `walk`    | 结构块 token 流迭代器                  |
// | `cell`    | DTB cell 读取工具函数                  |
// | `resolve` | 字符串块安全解析（内部模块）            |

pub mod cell;
pub mod header;
mod resolve; // crate-internal: walk.rs 使用
pub mod walk;

pub use header::FdtHeader;
pub use walk::{FdtIter, Token};
