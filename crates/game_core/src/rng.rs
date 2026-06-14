//! 自带的确定性 PRNG（splitmix64），让 game_core 保持零依赖。
//!
//! 用同一个 seed 就能复现整局发牌，方便测试和（将来）联机时
//! 服务器/客户端对齐随机结果。

pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 返回 [0, n) 范围内的整数。
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// 原地 Fisher–Yates 洗牌。
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.below(i + 1);
            slice.swap(i, j);
        }
    }
}
