# RiverBorn 服务器（阶段 1：大厅）

权威服务器，跑在公网机器上。客户端用 `bevy_renet` 连它。
传输：renet 1.0（UDP / netcode），消息协议：`riverborn_net`（JSON）。

## 本地两个客户端联调

1. 起服务器（监听 UDP 7777）：
   ```
   cargo run -p riverborn_server
   # 或指定端口： cargo run -p riverborn_server 7777
   ```
2. 起客户端，让它连本地服务器（默认连公网 IP，用**命令行参数**指定地址；
   WSL 跑 Windows exe 时 `VAR=值 cargo.exe` 那种环境变量前缀不会透传，所以用参数）：
   ```
   cargo run -p riverborn -- 127.0.0.1:7777
   ```
   开两个这样的客户端窗口，分别进「Multiplayer」→ 输名字 → 一个「创建房间」、另一个「刷新列表」看到房间号后输入加入，即可在同一房间互相看到。

## 部署到你的公网机器

服务器是纯 Linux 程序（game_core 零依赖 + renet）。最省事在机器上直接编译：

```bash
# 在公网机器上（已装 rust）
git clone <你的仓库> && cd Riverborn
cargo build --release -p riverborn_server
# 常驻运行（tmux 或 systemd）
tmux new -s rb 'target/release/riverborn_server 7777'
```

**防火墙**：放行 **UDP 7777**（云厂商安全组也要开）。SSH 在 443，游戏服用别的端口即可。

客户端连接：把客户端的 `server_addr()` 默认值或 `RIVERBORN_SERVER` 指到 `你的公网IP:7777`。

> 阶段 2 会在房间里真正发牌跑 `game_core::Match`，下发裁剪隐藏信息的对局视图，复用现有牌桌 UI。
