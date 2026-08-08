// 简单交互 shell — 内核任务，阻塞读 console 输入，解析并执行内置命令
//
// 消费 console::read（阻塞读）+ console::print（输出）+ clock（uptime）+
// platform（meminfo）+ hal::rtc（date）+ sbi（shutdown）+ schedule/demos
// （bench：按 DEMO_* 开关 spawn 演示任务集）。spawn 后常驻：打印提示符 →
// 阻塞读一行（\r/\n 结束，退格编辑）→ 解析 → 执行 → 循环。系统不再"跑完
// demo 自动关机"——shell 是默认交互，`bench` 命令按 DEMO_* 编译期开关
// 重放演示任务集（见 demos.rs）。
//
// 依赖方向：shell → console::read/print + clock + platform + hal::rtc + sbi
// + schedule + demos（组合层：组装原子/服务能力成交互流程，不反向依赖）。
//
// 回显说明：输入字符已由中断层 InputHandler 回显（\r 特判），shell 只需
// 处理行编辑语义（退格擦除）与回车后的换行补全。

use alloc::vec::Vec;

use crate::{
    demos,
    schedule::{Entry, TaskBuilder},
};

/// shell 主循环 — 常驻任务入口（`Entry::Kernel`）。
///
/// 无输入设备（`read_byte` 返回 NotFound）时直接返回：无交互意义，任务
/// 退出后系统走"全部任务结束"路径。
pub fn run() {
    loop {
        print!("atom> ");
        let Some(line) = read_line() else {
            return;
        };
        execute(&line);
    }
}

/// 阻塞读取一行（`\r` 或 `\n` 结束），返回行字节（不含结束符）。
///
/// 退格（0x08 / 0x7f）删除末字符并输出擦除序列；可打印 ASCII 追加（中断层
/// 已回显）；其余控制字符忽略。输入设备缺失 → None。
fn read_line() -> Option<Vec<u8>> {
    let mut line = Vec::with_capacity(32);
    loop {
        let c = crate::io::stdin().read_byte().ok()?;
        match c {
            b'\r' | b'\n' => return Some(line), // 回车结束（\r 已回显，光标在行首）
            b'\x08' | b'\x7f' => {
                // 退格：删除末字符并擦除显示（覆盖 \b \b）
                if line.pop().is_some() {
                    print!("\x08 \x08");
                }
            }
            c if (0x20..=0x7e).contains(&c) => {
                line.push(c);
            }
            _ => {} // 其他控制字符忽略
        }
    }
}

/// 解析并执行一行命令（首个空白分隔词为命令，其余为参数）。
fn execute(line: &[u8]) {
    println!(); // 回车后补换行（光标回行首 → 下一行）
    let line = core::str::from_utf8(line).unwrap_or("");
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else {
        return; // 空行
    };
    let args: Vec<&str> = parts.collect();
    match cmd {
        "help" => help(),
        "echo" => println!("{}", args.join(" ")),
        "version" | "ver" => println!("atom kernel {}", env!("CARGO_PKG_VERSION")),
        "uptime" => {
            let ticks = crate::clock::now();
            println!(
                "up {} ticks ({} ms)",
                ticks,
                crate::clock::ticks_to_usecs(ticks) / 1000
            );
        }
        "date" => match crate::hal::rtc::epoch_secs() {
            Some(s) => println!("epoch {s} s since Unix epoch"),
            None => println!("no RTC registered"),
        },
        "clear" => print!("\x1b[2J\x1b[H"), // ANSI 清屏
        "meminfo" => {
            let cfg = crate::platform::get();
            println!(
                "DRAM {:#x}..{:#x} ({} MiB), timebase {} Hz",
                cfg.dram_base,
                cfg.dram_base + cfg.dram_size,
                cfg.dram_size / (1024 * 1024),
                cfg.timebase_frequency,
            );
        }
        "shutdown" | "poweroff" => {
            println!("shutting down");
            crate::sbi::system_reset(crate::sbi::RESET_TYPE_SHUTDOWN, 0);
        }
        "bench" => {
            // 按 DEMO_* 编译期开关运行全部演示任务。demos::run 自身只负责
            // spawn 子任务后退出，子任务与 shell 并行运行（日志可能交错）。
            println!("running demos (DEMO_* compile switches)...");
            TaskBuilder::new(Entry::Kernel(demos::run)).spawn();
        }
        // ── M4 文件系统命令（/data 子树 + /dev 静态树）──
        "ls" => {
            let path = args.first().copied().unwrap_or("/data");
            match crate::file::open(path, crate::file::OpenFlags::READ) {
                Ok(fd) => {
                    let mut buf = [0u8; 1024];
                    match crate::file::readdir(fd, &mut buf) {
                        // readdir 输出已是 "name\n" 格式，整块打印
                        Ok(n) => print!("{}", core::str::from_utf8(&buf[..n]).unwrap_or("")),
                        Err(e) => println!("ls: {e:?}"),
                    }
                    let _ = crate::file::close(fd);
                }
                Err(e) => println!("ls: {e:?}"),
            }
        }
        "cat" => {
            let Some(path) = args.first() else {
                println!("usage: cat <path>");
                return;
            };
            match crate::file::open(path, crate::file::OpenFlags::READ) {
                Ok(fd) => {
                    let mut buf = [0u8; 256];
                    loop {
                        match crate::file::read(fd, &mut buf) {
                            Ok(0) => break, // EOF
                            Ok(n) => print!("{}", alloc::string::String::from_utf8_lossy(&buf[..n])),
                            Err(_) => break,
                        }
                    }
                    let _ = crate::file::close(fd);
                }
                Err(e) => println!("cat: {e:?}"),
            }
        }
        "write" => {
            let Some(path) = args.first() else {
                println!("usage: write <path> <content...>");
                return;
            };
            let content = args[1..].join(" ");
            // 打开已有文件；不存在 → create（O_CREAT 语义，仅动态目录 /data 支持）
            let fd = match crate::file::open(path, crate::file::OpenFlags::WRITE) {
                Ok(fd) => fd,
                Err(_) => match crate::file::create(path, crate::file::OpenFlags::WRITE) {
                    Ok(fd) => fd,
                    Err(e) => {
                        println!("write: create failed: {e:?}");
                        return;
                    }
                },
            };
            match crate::file::write(fd, content.as_bytes()) {
                Ok(n) => println!("wrote {n} bytes"),
                Err(e) => println!("write: {e:?}"),
            }
            let _ = crate::file::close(fd);
        }
        _ => println!("unknown command '{cmd}' — type 'help' for usage"),
    }
}

/// 内置命令列表。
fn help() {
    println!("atom shell — builtin commands:");
    println!("  help                 show this help");
    println!("  echo <text>          print text");
    println!("  version              kernel version");
    println!("  uptime               clock ticks / ms since boot");
    println!("  date                 RTC epoch seconds (if registered)");
    println!("  clear                ANSI clear screen");
    println!("  meminfo              DRAM layout");
    println!("  shutdown             power off (SBI)");
    println!("  bench                run demo task set (DEMO_* switches)");
    println!("  ls [path]            list directory (default /data)");
    println!("  cat <path>           print file contents");
    println!("  write <path> <text>  write text to file (creates if absent)");
}
