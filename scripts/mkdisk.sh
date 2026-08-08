#!/usr/bin/env bash
# 创建/重置 M4 验收用磁盘镜像（QEMU virtio-blk 的 host 侧 backing file）。
# 磁盘持久于 host：写盘数据重启后仍在——持久性验收靠两次 QEMU 重启读回一致。
# 注意：先 rm 再 truncate——truncate 对已有文件只改大小不清内容，残留旧 FS 数据。
set -euo pipefail
cd "$(dirname "$0")/.."

rm -f disk.img
truncate -s 32M disk.img
echo "created fresh disk.img (32 MiB) — 加 -drive file=disk.img,if=virtio,format=raw 启动"
