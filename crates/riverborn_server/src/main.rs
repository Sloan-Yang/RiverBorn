//! RiverBorn 权威服务器（headless）。
//!
//! 阶段 1：大厅。管理连接、房间（建/列/进/退）、开始游戏（占位）。
//! 用裸 renet（UDP/netcode）做传输，消息走 `riverborn_net` 协议（JSON）。
//! 阶段 2 再在每个房间里跑 `game_core::Match`，下发裁剪过隐藏信息的对局视图。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use renet::{ConnectionConfig, DefaultChannel, RenetServer, ServerEvent};
use renet_netcode::{NetcodeServerTransport, ServerAuthentication, ServerConfig};
use riverborn_net::{ClientMsg, LobbyPlayer, RoomInfo, RoomState, ServerMsg, PROTOCOL_ID, DEFAULT_PORT};

const CHANNEL: DefaultChannel = DefaultChannel::ReliableOrdered;
const MAX_NAME: usize = 16;

/// 一名已连接玩家。
struct Conn {
    name: String,
    room: Option<String>,
}

/// 一个房间。
struct Room {
    code: String,
    name: String,
    max_players: usize,
    fill_ai: bool,
    host: u64,
    members: Vec<u64>, // 按加入顺序
    in_game: bool,
}

#[derive(Default)]
struct Lobby {
    conns: HashMap<u64, Conn>,
    rooms: HashMap<String, Room>,
    seq: u64, // 房间码生成用的递增计数
}

fn main() {
    let port = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_PORT);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let socket = UdpSocket::bind(addr).expect("绑定 UDP 端口失败");
    let current_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let server_config = ServerConfig {
        current_time,
        max_clients: 64,
        protocol_id: PROTOCOL_ID,
        public_addresses: vec![addr],
        authentication: ServerAuthentication::Unsecure,
    };
    let mut transport = NetcodeServerTransport::new(server_config, socket).expect("创建 netcode transport 失败");
    let mut server = RenetServer::new(ConnectionConfig::default());
    let mut lobby = Lobby::default();

    println!("RiverBorn 服务器已启动，监听 UDP {addr}（协议 {PROTOCOL_ID:#x}）");

    let mut last = Instant::now();
    loop {
        let now = Instant::now();
        let dt = now - last;
        last = now;

        server.update(dt);
        transport.update(dt, &mut server).ok();

        while let Some(event) = server.get_event() {
            match event {
                ServerEvent::ClientConnected { client_id } => lobby.on_connect(client_id, &mut server),
                ServerEvent::ClientDisconnected { client_id, .. } => lobby.on_disconnect(client_id, &mut server),
            }
        }

        for client_id in server.clients_id() {
            while let Some(bytes) = server.receive_message(client_id, CHANNEL) {
                if let Some(msg) = ClientMsg::decode(&bytes) {
                    lobby.handle(client_id, msg, &mut server);
                }
            }
        }

        transport.send_packets(&mut server);
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn send(server: &mut RenetServer, client: u64, msg: &ServerMsg) {
    server.send_message(client, CHANNEL, msg.encode());
}

impl Lobby {
    fn on_connect(&mut self, client: u64, server: &mut RenetServer) {
        self.conns.insert(client, Conn { name: format!("Player{client}"), room: None });
        send(server, client, &ServerMsg::Welcome { your_id: client });
        println!("+ 玩家 {client} 已连接");
    }

    fn on_disconnect(&mut self, client: u64, server: &mut RenetServer) {
        self.leave_room(client, server);
        self.conns.remove(&client);
        println!("- 玩家 {client} 已断开");
    }

    fn handle(&mut self, client: u64, msg: ClientMsg, server: &mut RenetServer) {
        match msg {
            ClientMsg::Hello { name } => {
                if let Some(c) = self.conns.get_mut(&client) {
                    c.name = sanitize_name(&name);
                }
            }
            ClientMsg::CreateRoom { room_name, max_players, fill_ai } => {
                self.leave_room(client, server); // 先退出旧房间
                let code = self.gen_code();
                let max_players = max_players.clamp(2, 6);
                self.rooms.insert(
                    code.clone(),
                    Room {
                        code: code.clone(),
                        name: sanitize_name(&room_name),
                        max_players,
                        fill_ai,
                        host: client,
                        members: vec![client],
                        in_game: false,
                    },
                );
                if let Some(c) = self.conns.get_mut(&client) {
                    c.room = Some(code.clone());
                }
                let state = self.room_state(&code);
                send(server, client, &ServerMsg::Joined { room: state });
                println!("房间 {code} 已创建（房主 {client}）");
            }
            ClientMsg::ListRooms => {
                let rooms: Vec<RoomInfo> = self
                    .rooms
                    .values()
                    .filter(|r| !r.in_game && r.members.len() < r.max_players)
                    .map(|r| RoomInfo {
                        code: r.code.clone(),
                        name: r.name.clone(),
                        players: r.members.len(),
                        max_players: r.max_players,
                        in_game: r.in_game,
                    })
                    .collect();
                send(server, client, &ServerMsg::RoomList { rooms });
            }
            ClientMsg::JoinRoom { code } => {
                let code = code.trim().to_uppercase();
                match self.rooms.get_mut(&code) {
                    None => send(server, client, &ServerMsg::Error { text: "房间号不存在".into() }),
                    Some(r) if r.in_game => send(server, client, &ServerMsg::Error { text: "该房间已开始游戏".into() }),
                    Some(r) if r.members.len() >= r.max_players => {
                        send(server, client, &ServerMsg::Error { text: "房间已满".into() })
                    }
                    Some(_) => {
                        self.leave_room(client, server);
                        // leave_room 可能借走 self，这里重新取。
                        if let Some(r) = self.rooms.get_mut(&code) {
                            if !r.members.contains(&client) {
                                r.members.push(client);
                            }
                        }
                        if let Some(c) = self.conns.get_mut(&client) {
                            c.room = Some(code.clone());
                        }
                        let state = self.room_state(&code);
                        send(server, client, &ServerMsg::Joined { room: state.clone() });
                        self.broadcast_except(&code, client, &ServerMsg::RoomUpdate { room: state }, server);
                        println!("玩家 {client} 加入房间 {code}");
                    }
                }
            }
            ClientMsg::LeaveRoom => {
                self.leave_room(client, server);
                send(server, client, &ServerMsg::Left);
            }
            ClientMsg::StartGame => {
                let room_code = self.conns.get(&client).and_then(|c| c.room.clone());
                let Some(code) = room_code else { return };
                let is_host = self.rooms.get(&code).map(|r| r.host == client).unwrap_or(false);
                if !is_host {
                    send(server, client, &ServerMsg::Error { text: "只有房主能开始游戏".into() });
                    return;
                }
                if let Some(r) = self.rooms.get_mut(&code) {
                    r.in_game = true;
                }
                let state = self.room_state(&code);
                // 阶段 2：这里改成真正发牌并下发对局视图。
                self.broadcast(&code, &ServerMsg::GameStarting, server);
                self.broadcast(&code, &ServerMsg::RoomUpdate { room: state }, server);
                println!("房间 {code} 开始游戏");
            }
        }
    }

    /// 把玩家从其所在房间移除；房间空了删除，房主走了改派。广播更新。
    fn leave_room(&mut self, client: u64, server: &mut RenetServer) {
        let Some(code) = self.conns.get_mut(&client).and_then(|c| c.room.take()) else {
            return;
        };
        let mut deleted = false;
        if let Some(r) = self.rooms.get_mut(&code) {
            r.members.retain(|&m| m != client);
            if r.members.is_empty() {
                deleted = true;
            } else if r.host == client {
                r.host = r.members[0];
            }
        }
        if deleted {
            self.rooms.remove(&code);
        } else if self.rooms.contains_key(&code) {
            let state = self.room_state(&code);
            self.broadcast(&code, &ServerMsg::RoomUpdate { room: state }, server);
        }
    }

    fn room_state(&self, code: &str) -> RoomState {
        let r = &self.rooms[code];
        RoomState {
            code: r.code.clone(),
            name: r.name.clone(),
            max_players: r.max_players,
            fill_ai: r.fill_ai,
            host: r.host,
            players: r
                .members
                .iter()
                .map(|&id| LobbyPlayer {
                    id,
                    name: self.conns.get(&id).map(|c| c.name.clone()).unwrap_or_default(),
                })
                .collect(),
            in_game: r.in_game,
        }
    }

    fn broadcast(&self, code: &str, msg: &ServerMsg, server: &mut RenetServer) {
        if let Some(r) = self.rooms.get(code) {
            for &m in &r.members {
                send(server, m, msg);
            }
        }
    }

    fn broadcast_except(&self, code: &str, except: u64, msg: &ServerMsg, server: &mut RenetServer) {
        if let Some(r) = self.rooms.get(code) {
            for &m in &r.members {
                if m != except {
                    send(server, m, msg);
                }
            }
        }
    }

    /// 生成一个不重复的 4 位大写字母房间码。
    fn gen_code(&mut self) -> String {
        loop {
            self.seq = self.seq.wrapping_add(1);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
            let mut r = game_core::rng::Rng::new(nanos ^ self.seq.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let code: String = (0..4)
                .map(|_| (b'A' + (r.next_u64() % 26) as u8) as char)
                .collect();
            if !self.rooms.contains_key(&code) {
                return code;
            }
        }
    }
}

fn sanitize_name(s: &str) -> String {
    let t: String = s.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    if t.is_empty() {
        "Player".into()
    } else {
        t
    }
}
