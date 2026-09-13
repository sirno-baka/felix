use crate::io::{inb, outb};
use crate::sync::mutex::Mutex;
use core::arch::asm;

/// Small kernel CSPRNG used by getrandom(2).
///
/// The generator itself is ChaCha20. On the old i686 hardware Felix targets
/// there is no architectural hardware RNG, so its initial seed is collected
/// from high-resolution timing jitter (TSC + PIT), RTC/PIT time and address
/// variation. This is sufficient for bringing up the userspace TLS stack, but
/// should be replaced/augmented with a real hardware entropy source before
/// treating Felix TLS keys as production-grade secrets.
struct RandomState {
    key: [u32; 8],
    counter: u64,
    nonce: [u32; 3],
    initialized: bool,
}

impl RandomState {
    const fn new() -> Self {
        Self {
            key: [0; 8],
            counter: 0,
            nonce: [0; 3],
            initialized: false,
        }
    }

    fn initialize(&mut self) {
        let mut seed = collect_seed();
        for word in &mut self.key {
            let value = splitmix64(&mut seed);
            *word = (value as u32) ^ ((value >> 32) as u32);
        }
        let n0 = splitmix64(&mut seed);
        let n1 = splitmix64(&mut seed);
        self.nonce = [n0 as u32, (n0 >> 32) as u32, n1 as u32];
        self.counter = splitmix64(&mut seed);
        self.initialized = true;
    }

    fn stir_timing(&mut self) {
        let t = rdtsc();
        let pit = pit_counter() as u64;
        let mix = t ^ pit.rotate_left(17) ^ crate::time::uptime_ms().rotate_left(31);
        self.key[0] ^= mix as u32;
        self.key[3] ^= (mix >> 32) as u32;
        self.key[5] = self.key[5].wrapping_add((mix.rotate_left(11)) as u32);
        self.counter = self.counter.wrapping_add(mix | 1);
    }

    fn block(&mut self) -> [u8; 64] {
        let counter_lo = self.counter as u32;
        let counter_hi = (self.counter >> 32) as u32;
        let input = [
            0x6170_7865,
            0x3320_646e,
            0x7962_2d32,
            0x6b20_6574,
            self.key[0],
            self.key[1],
            self.key[2],
            self.key[3],
            self.key[4],
            self.key[5],
            self.key[6],
            self.key[7],
            counter_lo,
            counter_hi ^ self.nonce[0],
            self.nonce[1],
            self.nonce[2],
        ];

        let mut x = input;
        for _ in 0..10 {
            quarter_round(&mut x, 0, 4, 8, 12);
            quarter_round(&mut x, 1, 5, 9, 13);
            quarter_round(&mut x, 2, 6, 10, 14);
            quarter_round(&mut x, 3, 7, 11, 15);
            quarter_round(&mut x, 0, 5, 10, 15);
            quarter_round(&mut x, 1, 6, 11, 12);
            quarter_round(&mut x, 2, 7, 8, 13);
            quarter_round(&mut x, 3, 4, 9, 14);
        }

        let mut out = [0u8; 64];
        for i in 0..16 {
            let word = x[i].wrapping_add(input[i]).to_le_bytes();
            out[i * 4..i * 4 + 4].copy_from_slice(&word);
        }
        self.counter = self.counter.wrapping_add(1);
        out
    }

    fn fill(&mut self, output: &mut [u8]) {
        if !self.initialized {
            self.initialize();
        }
        self.stir_timing();

        let mut offset = 0usize;
        while offset < output.len() {
            let block = self.block();
            let count = (output.len() - offset).min(block.len());
            output[offset..offset + count].copy_from_slice(&block[..count]);
            offset += count;
        }

        // Forward-security rekey: do not leave the exact key that generated the
        // returned stream in memory for the next request.
        let rekey = self.block();
        for i in 0..8 {
            self.key[i] ^= u32::from_le_bytes([
                rekey[i * 4],
                rekey[i * 4 + 1],
                rekey[i * 4 + 2],
                rekey[i * 4 + 3],
            ]);
        }
    }
}

static RNG: Mutex<RandomState> = Mutex::new(RandomState::new());

pub fn fill_bytes(bytes: &mut [u8]) {
    if bytes.is_empty() {
        return;
    }
    RNG.lock().fill(bytes);
}

fn quarter_round(x: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    let mut a0 = x[a];
    let mut b0 = x[b];
    let mut c0 = x[c];
    let mut d0 = x[d];

    a0 = a0.wrapping_add(b0);
    d0 ^= a0;
    d0 = d0.rotate_left(16);

    c0 = c0.wrapping_add(d0);
    b0 ^= c0;
    b0 = b0.rotate_left(12);

    a0 = a0.wrapping_add(b0);
    d0 ^= a0;
    d0 = d0.rotate_left(8);

    c0 = c0.wrapping_add(d0);
    b0 ^= c0;
    b0 = b0.rotate_left(7);

    x[a] = a0;
    x[b] = b0;
    x[c] = c0;
    x[d] = d0;
}

#[inline]
fn rdtsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | low as u64
}

#[inline]
fn pit_counter() -> u16 {
    // Latch channel 0, then read low/high bytes of the current countdown.
    outb(0x43, 0x00);
    let lo = inb(0x40) as u16;
    let hi = inb(0x40) as u16;
    lo | (hi << 8)
}

fn collect_seed() -> u64 {
    let marker = 0u8;
    let mut seed = rdtsc()
        ^ crate::time::realtime_ms().rotate_left(7)
        ^ crate::time::uptime_ms().rotate_left(29)
        ^ ((core::ptr::from_ref(&marker).addr() as u64).rotate_left(19));

    // Sample fine timing around I/O and deliberately variable spin counts.
    // The low TSC bits pick up interrupt/device/virtualization scheduling jitter.
    for i in 0..64u32 {
        let before = rdtsc();
        let pit = pit_counter() as u64;
        let spins = ((before ^ pit) & 0x1f) as usize + 1;
        for _ in 0..spins {
            core::hint::spin_loop();
        }
        let after = rdtsc();
        let sample = after
            ^ before.rotate_left((i & 31) + 1)
            ^ pit.rotate_left((i % 47) + 1)
            ^ (crate::time::jiffies() as u64).rotate_left((i % 59) + 1);
        seed ^= sample;
        seed = seed.rotate_left(17).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    if seed == 0 {
        0xD1B5_4A32_D192_ED03
    } else {
        seed
    }
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
