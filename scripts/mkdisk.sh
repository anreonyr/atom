#!/usr/bin/env bash
# 创建 M4 验收用磁盘镜像（QEMU virtio-blk 的 host 侧 backing file）。
# 磁盘持久于 host：写盘数据重启后仍在——持久性验收靠两次 QEMU 重启读回一致。
set -euo pipefail
cd "$(dirname "$0")/.."

truncate -s 32M disk.img
echo "created disk.img (32 MiB) — 加 -drive file=disk.img,if=virtio,format=raw 启动"
