#!/bin/bash

set -e

if [ "$EUID" -ne 0 ]; then
    echo "请用 sudo 运行: sudo bash init.sh"
    exit 1
fi

echo "=== 初始化 cb rootfs ==="

mkdir -p /var/lib/cb/rootfs/{bin,etc,proc,dev,tmp,sys,usr/bin}
mkdir -p /var/lib/cb/containers

BUSYBOX=/var/lib/cb/rootfs/bin/busybox
if [ ! -f "$BUSYBOX" ]; then
    echo "正在下载 busybox..."
    wget -q --show-progress -O "$BUSYBOX" \
        https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox
    chmod +x "$BUSYBOX"
    cd /var/lib/cb/rootfs/bin && ./busybox --install . && cd -
    echo "busybox 安装完成"
else
    echo "busybox 已存在，跳过"
fi

echo ""
echo "=== 完成 ==="
echo "rootfs 位于: /var/lib/cb/rootfs"
echo ""
echo "启动 daemon: sudo ./target/debug/cb daemon"
echo "运行容器: sudo ./target/debug/cb run ls 128M"
