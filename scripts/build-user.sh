#!/usr/bin/env bash
# 构建用户程序 ELF（M1 loader 验收探针 + M5 spawn 子程序）并复制为 user.elf。
# 改动 user/src 或 user/link.ld 后需重跑本脚本——user.elf 经内核
# include_bytes! 嵌入，漂移会让人看到旧程序。
#
# workspace 化后在根目录运行（target 上提到根 target/）；-Tlink.ld 经各 crate
# build.rs 传绝对路径，无需 cd user。
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=target/riscv64gc-unknown-none-elf/release

# 两段构建：spawned（M5 子程序）先产出，user 的 include_bytes! 需要它的
# 产物存在（cargo 不保证 bin 间构建顺序）。spawned.elf 被 user bin
# include_bytes! 追踪（rustc dep-info），更新自动触发重编译；build.rs 的
# rerun-if-changed=link.ld 覆盖链接脚本变化，无需 touch。
cargo build -p user --release --bin spawned
cp "$TARGET/spawned" user/spawned.elf

cargo build -p user --release --bin user
cp "$TARGET/user" user/user.elf

echo "built user/user.elf (embeds spawned.elf):"
if command -v readelf >/dev/null 2>&1; then
  # LC_ALL=C：readelf 输出语言随 locale 变化，固定英文便于 grep
  LC_ALL=C readelf -h user/user.elf | grep -E 'Type:|Machine:|Entry point'
  echo "---"
  LC_ALL=C readelf -l user/user.elf | grep -E 'LOAD|INTERP'
  echo "--- spawned.elf:"
  LC_ALL=C readelf -h user/spawned.elf | grep -E 'Type:|Machine:|Entry point'
else
  echo "(readelf 不可用，跳过校验)"
fi
