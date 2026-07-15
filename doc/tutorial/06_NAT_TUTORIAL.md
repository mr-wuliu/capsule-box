# NAT 教程（第 20 轮）：让容器访问外网

> 前置：你已经完成 NETWORK_TUTORIAL（第 18-19 轮），容器现在拥有独立的网络命名空间、
> 启用了回环接口，并通过 veth pair 和宿主机互通（容器能 `ping 10.0.0.1`）。
>
> 本轮的目标：让容器能访问**公网**（`ping 8.8.8.8`，乃至 `ping google.com`）。

---

## 现状回顾：容器为什么还上不了网

第 19 轮结束时，网络拓扑是这样的：

```
宿主机 netns                              容器 netns
┌────────────────────────┐              ┌────────────────────────┐
│  eth0（真实网卡，能上网） │              │                        │
│                         │              │                        │
│  v<id>  10.0.0.1/24 ────┼──虚拟网线────┼──── c<id>  10.0.0.2/24  │
└────────────────────────┘              └────────────────────────┘
```

容器发往 `10.0.0.1` 的包能通，但发往 `8.8.8.8` 的包出不去。原因有三个，缺一不可：

1. **宿主机不肯转发**：Linux 默认不会替别的机器转发数据包（`ip_forward = 0`）。容器的包到了宿主机 `v<id>`，宿主机看它目标不是自己，直接丢弃。
2. **没有做地址转换（NAT）**：就算宿主机肯转发，容器的源地址是 `10.0.0.2`——这是一个**私有地址**，外网的服务器收到后，回包不知道往哪送（`10.0.0.2` 在公网上不可路由）。
3. **不会域名解析（DNS）**：容器 rootfs 里没有 `/etc/resolv.conf`，所以只能 `ping IP`，不能 `ping 域名`。

本轮就是逐一解决这三点。

---

## 概念一：IP 转发（IP forwarding）

一台主机默认只处理"目标是自己"的包。当它收到一个"目标是别人"的包时，要不要帮忙转发出去，由内核参数 `net.ipv4.ip_forward` 决定：

- `0`（默认）：不转发，直接丢弃 → 容器的包到宿主机就"断头"了
- `1`：开启转发 → 宿主机会像路由器一样，把包转发到通往目标的网卡

所以第一件事就是打开它：

```bash
echo 1 > /proc/sys/net/ipv4/ip_forward
```

---

## 概念二：NAT / MASQUERADE（地址伪装）

假设转发开了，容器的包（源地址 `10.0.0.2`）经宿主机 `eth0` 发到了 `8.8.8.8`。`8.8.8.8` 收到后要回包，可回包的目标是 `10.0.0.2`——这是个私有地址，公网根本没法把包送回来。

**NAT（网络地址转换）** 解决这个问题：宿主机在把包转发出去之前，把源地址**改写成自己的公网地址**（比如 `eth0` 的地址）。这样 `8.8.8.8` 的回包会先回到宿主机，宿主机再根据连接跟踪表把目标地址**改回** `10.0.0.2`，转发给容器。

```
出方向：
容器 10.0.0.2 ──► 宿主机（改源地址为 eth0 的IP）──► eth0 ──► 8.8.8.8

回方向：
8.8.8.8 ──► eth0（目标是宿主机IP）──► 宿主机（改目标回 10.0.0.2）──► 容器
```

`iptables` 里实现这个的规则是 **MASQUERADE**（伪装）：

```bash
iptables -t nat -A POSTROUTING -s 10.0.0.0/24 -j MASQUERADE
```

- `-t nat`：操作 NAT 表
- `-A POSTROUTING`：在"包即将离开宿主机"的位置挂规则
- `-s 10.0.0.0/24`：只对来自容器子网的包生效
- `-j MASQUERADE`：把源地址伪装成出口网卡的地址

> `MASQUERADE` 和 `SNAT` 效果类似，区别在于 `MASQUERADE` 会**自动**取出口网卡的当前地址，
> 不用手写死 IP，适合出口地址会变化（如拨号、DHCP）的场景。

此外，有些系统（比如装了 Docker 的机器）默认 `FORWARD` 链的策略是 `DROP`，需要显式放行容器子网的转发：

```bash
iptables -A FORWARD -s 10.0.0.0/24 -j ACCEPT
iptables -A FORWARD -d 10.0.0.0/24 -j ACCEPT
```

---

## 概念三：DNS

`ping 8.8.8.8` 通了，但 `ping google.com` 还是不行，因为容器不知道去哪查域名。给容器 rootfs 写一个最简单的 `/etc/resolv.conf` 即可：

```
nameserver 8.8.8.8
```

---

## 落地：在 `network.rs` 里加 NAT 和 DNS

先在文件顶部补一个导入（`setup_dns` 要用到 `Path`）：

```rust
// src/sandbox/network.rs —— 顶部
use std::path::Path;
```

然后新增三个函数。它们复用了第 19 轮已有的 `run_cmd` 辅助函数：

```rust
// src/sandbox/network.rs —— 新增

const CONTAINER_SUBNET: &str = "10.0.0.0/24";

/// 打开 IP 转发 + 配置 NAT。幂等：可安全重复调用。
pub fn setup_nat() -> Result<(), AppError> {
    // 1. 打开 IPv4 转发
    std::fs::write("/proc/sys/net/ipv4/ip_forward", "1")?;

    // 2. NAT：容器子网出去的包做源地址伪装
    ensure_iptables(&[
        "-t", "nat", "-A", "POSTROUTING",
        "-s", CONTAINER_SUBNET, "-j", "MASQUERADE",
    ])?;

    // 3. 放行转发（应对默认 FORWARD 策略为 DROP 的情况）
    ensure_iptables(&["-A", "FORWARD", "-s", CONTAINER_SUBNET, "-j", "ACCEPT"])?;
    ensure_iptables(&["-A", "FORWARD", "-d", CONTAINER_SUBNET, "-j", "ACCEPT"])?;

    Ok(())
}

/// 幂等地添加一条 iptables 规则：先用 -C 检查是否已存在，不存在才添加。
/// 避免每启动一个容器就重复插一条相同规则。
fn ensure_iptables(add_args: &[&str]) -> Result<(), AppError> {
    // 把动作 -A 换成 -C，用来"检查规则是否存在"
    let check_args: Vec<&str> = add_args
        .iter()
        .map(|a| if *a == "-A" { "-C" } else { *a })
        .collect();

    // iptables -C 成功（退出码 0）表示规则已存在
    let exists = Command::new("iptables")
        .args(&check_args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if exists {
        return Ok(());
    }
    run_cmd("iptables", add_args)
}

/// 往容器 rootfs 写 /etc/resolv.conf，让域名可以解析
pub fn setup_dns(merged: &Path) -> Result<(), AppError> {
    let resolv = merged.join("etc/resolv.conf");
    std::fs::write(resolv, "nameserver 8.8.8.8\n")?;
    Ok(())
}
```

---

## 接入 `start_container`

在第 19 轮里，父进程配好 veth 之后就 `notify(net_w)` 放行容器了。现在在 `notify` **之前**补上 NAT 和 DNS：

```rust
// src/sandbox/mod.rs —— start_container 的父进程分支

            // 配置veth
            network::setup_veth(&host_if, &cont_if, child_pid)?;

            // ← 新增：打开转发 + NAT + 写 DNS
            network::setup_nat()?;
            network::setup_dns(&merged)?;

            // 通知子进程网络就绪
            notify(net_w);
            close_fd(net_w);
```

`merged` 变量在 `start_container` 里已经有了（第 19 轮 `let merged = container_fs.merged.clone();`），直接传给 `setup_dns` 即可。

**为什么放在 `notify` 之前**：容器进程在收到 `net_ready` 信号后才会 `exec` 用户命令。把 NAT 和 DNS 都放在 `notify` 之前，就保证了"容器开始跑命令时，转发、NAT、DNS 全部就绪"。这正是第 19 轮建立的同步机制的价值。

---

## 关于清理：为什么本轮几乎不需要清理

第 19 轮预告里提过"NAT 规则要清理"。真正实现后会发现：**在当前设计下，几乎不需要清理**，原因值得讲清楚：

1. **veth 自动清理**：容器退出 → netns 销毁 → 容器端 `c<id>` 消失 → 宿主机端 `v<id>` 也随之消失。无需手动删。
2. **NAT / FORWARD 规则是共享的**：本项目所有容器共用 `10.0.0.0/24` 这一个子网，所以那几条 iptables 规则是**全局共享的基础设施**，加一次就够了。`ensure_iptables` 的幂等检查保证了不会重复堆积，所以也不用在容器退出时删。
3. **`ip_forward` 是全局开关**：打开后保持打开即可，是无害的系统设置。

**什么时候才需要按容器清理？** 如果将来给每个容器分配**独立子网**（比如容器 A 用 `10.0.1.0/24`、容器 B 用 `10.0.2.0/24`），那每个容器就会有自己专属的 NAT 规则，这时就必须在容器退出时删掉它。删除的钩子应该挂在第 16 轮的 `ContainerManager::on_container_exit`——也就是 SIGCHLD 处理路径里，容器一退出就撤销它的规则。

> 换句话说：**共享资源加一次、幂等；专属资源用完即删**。这是资源生命周期管理的通用原则。

---

## 验证

```bash
# 确保宿主机装了 iptables
which iptables

# 重新编译 + 重启 daemon（务必重启！）
cargo build
sudo ./target/debug/mybox daemon
```

在另一个终端依次执行（输出看 daemon 终端）：

```bash
# 1. 访问公网 IP —— 现在应该通了
./target/debug/mybox run ping -c 2 8.8.8.8 128M
# daemon 终端：64 bytes from 8.8.8.8: seq=0 ...

# 2. 域名解析 —— DNS 生效后也通
./target/debug/mybox run ping -c 2 google.com 128M
# daemon 终端：PING google.com (142.250.x.x) ...
```

如果两条都通，容器就真正接入互联网了。

---

## WSL2 注意事项

WSL2 本身就跑在一层 NAT 之后（Windows 主机给 WSL 做了地址转换）。我们在 WSL 内部再加一层容器 NAT，形成"容器 → WSL → Windows → 外网"的两级转换，通常能正常工作，但有两点要留意：

- **iptables 后端**：较新的发行版可能用 `nftables` 兼容层。若 `iptables` 命令报错，检查 `iptables --version` 是否显示 `nf_tables`，必要时用 `iptables-legacy`。
- **`FORWARD` 默认策略**：如果宿主机装了 Docker，`FORWARD` 链默认 `DROP`，本轮的两条 `FORWARD ACCEPT` 规则正是为此准备的，务必保留。

---

## 本轮收获

- **`ip_forward`**：内核默认不替他人转发数据包，做网关必须先打开它
- **NAT / MASQUERADE**：把容器私有源地址伪装成宿主机地址，回包才能原路返回；`MASQUERADE` 会自动取出口网卡地址
- **`FORWARD` 链**：转发的包还要过 `FORWARD` 链，默认策略为 `DROP` 时需显式放行
- **DNS**：往容器 rootfs 写 `/etc/resolv.conf` 即可解析域名
- **幂等**：共享的 iptables 规则用 `iptables -C` 先检查再添加，避免重复堆积
- **资源生命周期**：共享资源加一次且幂等、专属资源用完即删；删除钩子应挂在 `on_container_exit`

---

## 第 21 轮（预告）：多容器与 IP 分配

现在有一个明显的限制：所有容器都被硬编码成宿主机端 `10.0.0.1`、容器端 `10.0.0.2`。同时启动**两个**容器时，两个宿主机端都想占用 `10.0.0.1` → 冲突。

下一轮要解决"多容器共存"：

1. **IP 地址分配**：给每个容器分配唯一的 `10.0.<n>.2/24`，宿主机端 `10.0.<n>.1`
2. **用 Linux bridge 替代点对点 veth**：所有容器接到一个虚拟网桥 `mybox0` 上，共享一个网段，容器之间也能互通（更接近 Docker 的 `bridge` 网络模型）
3. **地址回收**：容器退出后把分配出去的 IP / 编号收回，供后续复用——又一次用到 `on_container_exit`
