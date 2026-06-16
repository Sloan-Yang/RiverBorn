//! RiverBorn 权威服务器（headless）。
//!
//! 管理连接、房间（大厅）、以及**对局**：每个房间跑一个 `game_core::Match`，
//! 空座位用 `ai::decide` 填 AI；给每个客户端下发**裁剪过隐藏信息**的 `GameView`
//! （只含它自己的底牌，摊牌后才亮全部）。传输用裸 renet（UDP/netcode），
//! 协议走 `riverborn_net`（JSON）。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use game_core::{Action, Adventurer, CommunityCard, Match, PlayerId, SeatDef};
use renet::{ConnectionConfig, DefaultChannel, RenetServer, ServerEvent};
use renet_netcode::{NetcodeServerTransport, ServerAuthentication, ServerConfig};
use riverborn_net::{
    CardView, ClientMsg, GameView, LobbyPlayer, RoomInfo, RoomState, SeatView, ServerMsg, DEFAULT_PORT, PROTOCOL_ID,
};

const CHANNEL: DefaultChannel = DefaultChannel::ReliableOrdered;
const MAX_NAME: usize = 16;
const AI_DELAY: f32 = 0.8; // AI 行动间隔
const SELECT_TIMEOUT: f32 = 30.0; // 摊牌点选超时，超时自动选最优

/// 一名已连接玩家。
struct Conn {
    name: String,
    room: Option<String>,
}

/// 一个房间里正在跑的对局。
struct RoomGame {
    mtch: Match,
    /// 座位号 → 客户端 id（None = AI 座位）。
    seat_clients: Vec<Option<u64>>,
    host: u64,
    ai_timer: f32,
    /// 摊牌时各座位人类提交的手选（按座位号）。
    selections: Vec<Option<[usize; 3]>>,
    select_timer: f32,
    settled: bool,
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
    game: Option<RoomGame>,
}

#[derive(Default)]
struct Lobby {
    conns: HashMap<u64, Conn>,
    rooms: HashMap<String, Room>,
    seq: u64,
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

        lobby.tick_games(dt.as_secs_f32(), &mut server);

        transport.send_packets(&mut server);
        std::thread::sleep(Duration::from_millis(16));
    }
}

fn send(server: &mut RenetServer, client: u64, msg: &ServerMsg) {
    server.send_message(client, CHANNEL, msg.encode());
}

fn random_seed() -> u64 {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    game_core::rng::Rng::new(nanos ^ 0xD1CE_5EED_1234_ABCD).next_u64()
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
                self.leave_room(client, server);
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
                        game: None,
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
                match self.rooms.get(&code) {
                    None => send(server, client, &ServerMsg::Error { text: "房间号不存在".into() }),
                    Some(r) if r.in_game => send(server, client, &ServerMsg::Error { text: "该房间已开始游戏".into() }),
                    Some(r) if r.members.len() >= r.max_players => {
                        send(server, client, &ServerMsg::Error { text: "房间已满".into() })
                    }
                    Some(_) => {
                        self.leave_room(client, server);
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
            ClientMsg::StartGame => self.start_game(client, server),
            ClientMsg::Act { action } => self.on_act(client, action, server),
            ClientMsg::SelectCards { picks } => self.on_select(client, picks, server),
            ClientMsg::Borrow => self.on_borrow(client, server),
            ClientMsg::NextHand => self.on_next_hand(client, server),
        }
    }

    // ---- 对局 ----

    fn start_game(&mut self, client: u64, server: &mut RenetServer) {
        let Some(code) = self.room_code_of(client) else { return };
        let (is_host, members, max_players, fill_ai, already) = {
            let r = &self.rooms[&code];
            (r.host == client, r.members.clone(), r.max_players, r.fill_ai, r.game.is_some())
        };
        if !is_host {
            send(server, client, &ServerMsg::Error { text: "只有房主能开始游戏".into() });
            return;
        }
        if already {
            return;
        }
        // 座位 = 现有真人 + AI 补齐。
        let humans: Vec<(u64, String)> = members
            .iter()
            .map(|&id| (id, self.conns.get(&id).map(|c| c.name.clone()).unwrap_or_else(|| "Player".into())))
            .collect();
        let total = if fill_ai { max_players.max(humans.len()) } else { humans.len() }.clamp(2, 6);
        let mut defs = Vec::new();
        let mut seat_clients = Vec::new();
        for (id, name) in &humans {
            defs.push(SeatDef::new(name.clone(), false));
            seat_clients.push(Some(*id));
        }
        let mut ai_n = 0;
        while defs.len() < total {
            ai_n += 1;
            defs.push(SeatDef::new(format!("AI-{ai_n}"), true));
            seat_clients.push(None);
        }
        let mtch = Match::new(defs, random_seed());
        let game = RoomGame {
            mtch,
            seat_clients,
            host: client,
            ai_timer: 0.0,
            selections: vec![None; total],
            select_timer: 0.0,
            settled: false,
        };
        if let Some(r) = self.rooms.get_mut(&code) {
            r.game = Some(game);
            r.in_game = true;
        }
        self.broadcast(&code, &ServerMsg::GameStarting, server);
        self.broadcast_views(&code, server);
        println!("房间 {code} 开始游戏（{total} 座，{} 真人）", humans.len());
    }

    fn on_act(&mut self, client: u64, action: Action, server: &mut RenetServer) {
        let Some(code) = self.room_code_of(client) else { return };
        let mut ok = false;
        if let Some(g) = self.rooms.get_mut(&code).and_then(|r| r.game.as_mut()) {
            let to_act = g.mtch.hand.to_act;
            let is_their_turn = g.seat_clients.get(to_act).copied().flatten() == Some(client);
            if is_their_turn && !g.mtch.hand_over() && g.mtch.hand.apply(action).is_ok() {
                g.ai_timer = 0.0;
                ok = true;
            }
        }
        if ok {
            self.broadcast_views(&code, server);
        }
    }

    fn on_select(&mut self, client: u64, picks: [usize; 3], server: &mut RenetServer) {
        let Some(code) = self.room_code_of(client) else { return };
        if let Some(g) = self.rooms.get_mut(&code).and_then(|r| r.game.as_mut()) {
            if let Some(seat) = g.seat_clients.iter().position(|c| *c == Some(client)) {
                let n = g.mtch.hand.community.len();
                let valid = picks.iter().all(|&i| i < n);
                if valid && g.mtch.hand_over() && !g.settled && g.mtch.hand.players[seat].is_in_hand() {
                    g.selections[seat] = Some(picks);
                }
            }
        }
        self.broadcast_views(&code, server);
    }

    fn on_borrow(&mut self, client: u64, server: &mut RenetServer) {
        let Some(code) = self.room_code_of(client) else { return };
        if let Some(g) = self.rooms.get_mut(&code).and_then(|r| r.game.as_mut()) {
            if let Some(seat) = g.seat_clients.iter().position(|c| *c == Some(client)) {
                g.mtch.borrow(seat);
            }
        }
        self.broadcast_views(&code, server);
    }

    fn on_next_hand(&mut self, client: u64, server: &mut RenetServer) {
        let Some(code) = self.room_code_of(client) else { return };
        if let Some(g) = self.rooms.get_mut(&code).and_then(|r| r.game.as_mut()) {
            if client == g.host && g.settled && g.mtch.hand_over() {
                g.mtch.next_hand();
                g.settled = false;
                let n = g.seat_clients.len();
                g.selections = vec![None; n];
                g.select_timer = 0.0;
                g.ai_timer = 0.0;
            }
        }
        self.broadcast_views(&code, server);
    }

    /// 推进所有房间的对局：AI 行动、摊牌结算。
    fn tick_games(&mut self, dt: f32, server: &mut RenetServer) {
        let codes: Vec<String> = self.rooms.keys().cloned().collect();
        for code in codes {
            let mut dirty = false;
            if let Some(g) = self.rooms.get_mut(&code).and_then(|r| r.game.as_mut()) {
                dirty = advance_game(g, dt);
            }
            if dirty {
                self.broadcast_views(&code, server);
            }
        }
    }

    fn broadcast_views(&self, code: &str, server: &mut RenetServer) {
        let Some(room) = self.rooms.get(code) else { return };
        let Some(g) = &room.game else { return };
        for &client in &room.members {
            let view = build_view(g, client);
            send(server, client, &ServerMsg::View { view });
        }
    }

    // ---- 房间/大厅 ----

    fn room_code_of(&self, client: u64) -> Option<String> {
        self.conns.get(&client).and_then(|c| c.room.clone())
    }

    fn leave_room(&mut self, client: u64, server: &mut RenetServer) {
        let Some(code) = self.conns.get_mut(&client).and_then(|c| c.room.take()) else {
            return;
        };
        let mut deleted = false;
        if let Some(r) = self.rooms.get_mut(&code) {
            r.members.retain(|&m| m != client);
            // 对局中离开：座位交给 AI 接管，游戏不卡住。
            if let Some(g) = &mut r.game {
                if let Some(seat) = g.seat_clients.iter().position(|c| *c == Some(client)) {
                    g.seat_clients[seat] = None;
                }
            }
            if r.members.is_empty() {
                deleted = true;
            } else if r.host == client {
                r.host = r.members[0];
                if let Some(g) = &mut r.game {
                    g.host = r.members[0];
                }
            }
        }
        if deleted {
            self.rooms.remove(&code);
        } else if self.rooms.contains_key(&code) {
            let state = self.room_state(&code);
            self.broadcast(&code, &ServerMsg::RoomUpdate { room: state }, server);
            self.broadcast_views(&code, server);
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

    fn gen_code(&mut self) -> String {
        loop {
            self.seq = self.seq.wrapping_add(1);
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
            let mut r = game_core::rng::Rng::new(nanos ^ self.seq.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let code: String = (0..4).map(|_| (b'A' + (r.next_u64() % 26) as u8) as char).collect();
            if !self.rooms.contains_key(&code) {
                return code;
            }
        }
    }
}

/// 推进一个对局：返回是否有状态变化（需要重广播视图）。
fn advance_game(g: &mut RoomGame, dt: f32) -> bool {
    if !g.mtch.hand_over() {
        // 下注阶段：轮到 AI 座位就延迟后自动行动；人类座位等其 Act 消息。
        let to_act = g.mtch.hand.to_act;
        let is_ai_seat = g.seat_clients.get(to_act).copied().flatten().is_none();
        if is_ai_seat {
            g.ai_timer += dt;
            if g.ai_timer >= AI_DELAY {
                g.ai_timer = 0.0;
                let action = game_core::ai::decide(&g.mtch.hand, to_act);
                let _ = g.mtch.hand.apply(action);
                return true;
            }
        }
        false
    } else if !g.settled {
        // 摊牌：等所有「在局的人类」提交手选，或超时。
        g.select_timer += dt;
        let all_selected = (0..g.seat_clients.len()).all(|i| {
            let human = g.seat_clients[i].is_some();
            let in_hand = g.mtch.hand.players[i].is_in_hand();
            !(human && in_hand) || g.selections[i].is_some()
        });
        if all_selected || g.select_timer >= SELECT_TIMEOUT {
            let picks: Vec<(PlayerId, [usize; 3])> = (0..g.seat_clients.len())
                .filter_map(|i| g.selections[i].map(|p| (g.mtch.hand.players[i].id, p)))
                .collect();
            g.mtch.settle_hand(&picks);
            g.settled = true;
            return true;
        }
        false
    } else {
        false
    }
}

/// 为某个客户端构造裁剪后的对局视图（只含它能看到的信息）。
fn build_view(g: &RoomGame, client: u64) -> GameView {
    let m = &g.mtch;
    let hand = &m.hand;
    let over = m.hand_over();
    let you_seat = g.seat_clients.iter().position(|c| *c == Some(client));

    let seats: Vec<SeatView> = (0..m.seats.len())
        .map(|i| {
            let s = &m.seats[i];
            let p = &hand.players[i];
            SeatView {
                name: s.name.clone(),
                chips: s.chips,
                debt: s.debt,
                committed: p.committed,
                status: p.status,
                is_turn: !over && i == hand.to_act,
                is_ai: g.seat_clients[i].is_none(),
                in_hand: p.is_in_hand(),
            }
        })
        .collect();

    let community: Vec<CardView> = hand.community.iter().map(community_card_view).collect();
    let dungeon_threats: Vec<u32> = hand
        .dungeon
        .iter()
        .map(|n| game_core::combat::node_threat(n, hand.aggro, hand.boss_berserk))
        .collect();
    let dungeon: Vec<CardView> = hand
        .dungeon
        .iter()
        .zip(&dungeon_threats)
        .map(|(n, t)| CardView { art: n.art.to_string(), label: format!("T{}  H{}", t, n.health) })
        .collect();

    let your_hole = you_seat.map(|i| adv_pair_view(&hand.players[i].hole));
    let revealed_holes: Vec<Option<[CardView; 2]>> = (0..m.seats.len())
        .map(|i| {
            if over && hand.players[i].is_in_hand() {
                Some(adv_pair_view(&hand.players[i].hole))
            } else {
                None
            }
        })
        .collect();

    let need_select = you_seat
        .map(|i| over && !g.settled && hand.players[i].is_in_hand() && g.selections[i].is_none())
        .unwrap_or(false);
    let can_next = over && g.settled && client == g.host;
    let low_chips = you_seat
        .map(|i| {
            let need = hand.current_bet.saturating_sub(hand.players[i].committed);
            m.seats[i].chips < need.max(100)
        })
        .unwrap_or(false);

    GameView {
        hand_no: m.hand_no,
        scene_label: m.scene.label().to_string(),
        scene_art: m.scene.art().to_string(),
        phase: hand.phase,
        pot: hand.pot,
        current_bet: hand.current_bet,
        ante: m.ante,
        aggro: hand.aggro,
        boss_berserk: hand.boss_berserk,
        you_seat,
        to_act_seat: if over { None } else { Some(hand.to_act) },
        seats,
        community,
        dungeon,
        dungeon_threats,
        your_hole,
        revealed_holes,
        result: m.last.clone(),
        need_select,
        can_next,
        low_chips,
    }
}

fn community_card_view(c: &CommunityCard) -> CardView {
    let label = match c {
        CommunityCard::Unit(a) => format!("P{}  H{}", a.power, a.health),
        CommunityCard::Equip(k) => k.tag(),
        CommunityCard::Consum(k) => k.tag().to_string(),
    };
    CardView { art: c.art().to_string(), label }
}

fn adv_pair_view(hole: &[Adventurer; 2]) -> [CardView; 2] {
    [adv_view(&hole[0]), adv_view(&hole[1])]
}

fn adv_view(a: &Adventurer) -> CardView {
    CardView { art: a.art.to_string(), label: format!("P{}  H{}", a.power, a.health) }
}

fn sanitize_name(s: &str) -> String {
    let t: String = s.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    if t.is_empty() {
        "Player".into()
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 个人类(client=1, 座位 0) + 1 个 AI(座位 1)。
    fn make_game() -> RoomGame {
        let defs = vec![SeatDef::new("You", false), SeatDef::new("AI-1", true)];
        RoomGame {
            mtch: Match::new(defs, 42),
            seat_clients: vec![Some(1u64), None],
            host: 1,
            ai_timer: 0.0,
            selections: vec![None; 2],
            select_timer: 0.0,
            settled: false,
        }
    }

    #[test]
    fn view_redacts_opponent_holes_until_showdown() {
        let g = make_game();
        let v = build_view(&g, 1); // 客户端 1 = 座位 0
        assert_eq!(v.you_seat, Some(0));
        assert!(v.your_hole.is_some(), "应能看到自己的底牌");
        assert!(v.revealed_holes.iter().all(|h| h.is_none()), "摊牌前看不到任何人底牌");
        assert_eq!(v.seats.len(), 2);
    }

    #[test]
    fn full_mixed_hand_settles_and_reveals() {
        let mut g = make_game();
        // 推进到摊牌：人类座位用 AI 逻辑代打，AI 座位走 advance_game（dt 超过延迟即触发）。
        let mut guard = 0;
        while !g.mtch.hand_over() && guard < 500 {
            let to_act = g.mtch.hand.to_act;
            if g.seat_clients[to_act].is_some() {
                let a = game_core::ai::decide(&g.mtch.hand, to_act);
                g.mtch.hand.apply(a).unwrap();
            } else {
                advance_game(&mut g, 1.0);
            }
            guard += 1;
        }
        assert!(g.mtch.hand_over(), "应推进到摊牌");

        // 摊牌：给在局人类填手选，advance_game 结算。
        let mut guard2 = 0;
        while !g.settled && guard2 < 50 {
            for i in 0..g.seat_clients.len() {
                if g.seat_clients[i].is_some() && g.mtch.hand.players[i].is_in_hand() && g.selections[i].is_none() {
                    g.selections[i] = Some([0, 1, 2]);
                }
            }
            advance_game(&mut g, 1.0);
            guard2 += 1;
        }
        assert!(g.settled, "应完成结算");

        let v = build_view(&g, 1);
        assert!(v.result.is_some(), "结算后视图应带结果");
        assert!(v.revealed_holes.iter().any(|h| h.is_some()), "摊牌后在局玩家底牌应揭示");

        // 推进下一手：重置选牌状态、phase 回到 PreFlop。
        g.mtch.next_hand();
        g.settled = false;
        g.selections = vec![None; 2];
        assert_eq!(g.mtch.hand.phase, game_core::Phase::PreFlop);
    }
}
