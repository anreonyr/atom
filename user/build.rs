fn main() {
    // 用户程序链接脚本（0x10000 基址，段页对齐）：workspace 化后 -Tlink.ld 不能
    // 放全局 config（kernel/user 各自不同），改由 build.rs 传绝对路径。
    let ld = format!("{}/link.ld", env!("CARGO_MANIFEST_DIR"));
    println!("cargo::rustc-link-arg=-T{ld}");
    println!("cargo::rerun-if-changed=link.ld"); // link.ld 变更自动重链
}
