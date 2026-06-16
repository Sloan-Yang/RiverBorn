# RiverBorn 服务器（大厅 + 联机对局）

权威服务器，跑在公网机器上。客户端用 `bevy_renet` 连它。
传输：renet 1.0（UDP / netcode），消息协议：`riverborn_net`（JSON）。

服务器在每个房间里跑 `game_core::Match`：空座位用 AI 补齐，逐手发牌/下注/摊牌结算，
给每个客户端下发**裁剪过隐藏信息**的对局视图（`GameView`，只含它自己的底牌，摊牌后才亮全部）。
玩家操作（下注 / 借锅 / 摊牌点选 / 下一手）发回服务器，由服务器验证并广播新状态。

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

客户端连接：默认连 `43.155.246.125:7777`，或 `cargo run -p riverborn -- 你的IP:7777`。

## 一局怎么玩

1. 房主在大厅选 2–6 人建房，朋友刷新列表/输房间号加入。
2. 房主点「开始」→ 服务器用真人 + AI 补齐到房间人数，发第一手。
3. 牌桌上：轮到你时 `Q`过 `W`跟 `E`加注 `R`弃 `T`全下；筹码不够点「借一锅」。
4. 摊牌时点选 3 张公共牌组队→「确认组队」；所有在局真人提交后服务器结算（AI 自动选最优）。
5. 结算计分板亮出所有人底牌；房主点「下一手」继续。

> 断线/离开房间时该座位自动交给 AI 接管，游戏不卡住。
