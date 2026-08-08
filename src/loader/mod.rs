// loader — ELF64 用户程序装载（组合模块）
//
// 职责：把无 libc 的 RISC-V 静态 ELF 解析 + 逐段装载进新地址空间，生产
// `(AddressSpace, entry)`——`TaskBuilder::loader` 消费它 spawn 出 U-mode 任务。
// 边界：不做 symbol/重定位解析（静态 ET_EXEC 无动态重定位）、不做 syscall/
// 进程管理。
//
// 分层：elf.rs（原子解析，零内存分配）+ load.rs（段装载，依赖 memory）。
// 依赖 memory；不依赖 schedule（无环）。

mod elf;
mod load;

pub use load::{LoadError, load};

/// M1 验收探针：内嵌的用户程序 ELF（静态、无 libc、ET_EXEC）。
///
/// 由 scripts/build-user.sh 构建（改动 `user/src` 或 `user/link.ld` 后需
/// 重跑，产物 `user/user.elf` 随内核 `include_bytes!` 嵌入）。
pub fn user_program() -> &'static [u8] {
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/user/user.elf"))
}
