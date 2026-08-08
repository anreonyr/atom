#!/usr/bin/env bash
# 构建用户程序 ELF（M1 loader 验收探针）并复制为 user/user.elf。
# 改动 user/src 或 user/link.ld 后需重跑本脚本——user.elf 经内核
# include_bytes! 嵌入，漂移会让人看到旧程序。
set -euo pipefail
cd "$(dirname "$0")/../user"

# cargo 不跟踪 link.ld 变化（-Tlink.ld 经 rustflags 传入，无 build.rs rerun）——
# touch 源文件强制重编译重链接，避免改链接脚本后产出陈旧 ELF
touch src/main.rs
cargo build --release
cp target/riscv64gc-unknown-none-elf/release/user user.elf

echo "built user/user.elf:"
if command -v readelf >/dev/null 2>&1; then
  # LC_ALL=C：readelf 输出语言随 locale 变化，固定英文便于 grep
  LC_ALL=C readelf -h user.elf | grep -E 'Type:|Machine:|Entry point'
  echo "---"
  LC_ALL=C readelf -l user.elf | grep -E 'LOAD|INTERP'
else
  echo "(readelf 不可用，跳过校验)"
fi
