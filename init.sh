#!/bin/bash

set -e

if [ "$EUID" -ne 0 ]; then
    echo "请用 sudo 运行: sudo bash init.sh"
    exit 1
fi

echo "=== 初始化 mybox rootfs ==="

mkdir -p /var/lib/mybox/rootfs/{bin,etc,proc,dev,tmp,sys,usr/bin}
mkdir -p /var/lib/mybox/containers

BUSYBOX=/var/lib/mybox/rootfs/bin/busybox
if [ ! -f "$BUSYBOX" ]; then
    echo "正在下载 busybox..."
    wget -q --show-progress -O "$BUSYBOX" \
        https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox
    chmod +x "$BUSYBOX"
    cd /var/lib/mybox/rootfs/bin && ./busybox --install . && cd -
    echo "busybox 安装完成"
else
    echo "busybox 已存在，跳过"
fi

echo ""
echo "=== 完成 ==="
echo "rootfs 位于: /var/lib/mybox/rootfs"
echo ""
echo "启动 daemon: sudo ./target/debug/mybox daemon"
echo "运行容器: sudo ./target/debug/mybox run ls 128M"
