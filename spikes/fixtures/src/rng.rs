/// SplitMix64 PRNG. Self-contained so fixtures never change with dependency
/// upgrades.
#[derive(Debug, Clone)]
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

    /// Uniform float in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in `[low, high)`.
    pub fn range(&mut self, low: u64, high: u64) -> u64 {
        assert!(low < high, "empty range {low}..{high}");
        low + self.next_u64() % (high - low)
    }

    /// Uniform index in `[0, len)`.
    pub fn index(&mut self, len: usize) -> usize {
        self.range(0, len as u64) as usize
    }

    pub fn chance(&mut self, probability: f64) -> bool {
        self.next_f64() < probability
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.index(items.len())]
    }

    /// Picks an item with probability proportional to its weight.
    pub fn weighted<'a, T>(&mut self, items: &'a [(T, u32)]) -> &'a T {
        let total: u64 = items.iter().map(|(_, weight)| u64::from(*weight)).sum();
        let mut roll = self.range(0, total);
        for (item, weight) in items {
            let weight = u64::from(*weight);
            if roll < weight {
                return item;
            }
            roll -= weight;
        }
        unreachable!("roll is below the total weight")
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.index(i + 1));
        }
    }

    /// Independent generator, so one stream's length doesn't shift another's.
    pub fn fork(&mut self) -> Rng {
        Rng::new(self.next_u64())
    }
}
