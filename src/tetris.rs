/// Shared abstractions for Tetris NEAT scenarios.
///
/// Both `nes-tetris` and `tetris-c` implement [`TetrisEngine`] and wrap it in
/// [`TetrisScenario`], which provides the eevee `Scenario` impl for free.
use eevee::{
    genome::Genome,
    network::{Continuous, ToNetwork},
    Connection, Network, Scenario,
};
use std::{
    marker::PhantomData,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};

pub const BOARD_SIZE: usize = 200;

// ---------------------------------------------------------------------------
// Engine trait
// ---------------------------------------------------------------------------

/// One live game.  Constructed fresh for each evaluation or exhibition run.
pub trait TetrisEngine {
    fn new_game(seed: u16, level: u8) -> Self
    where
        Self: Sized;
    /// Number of network output nodes this engine consumes.
    fn outputs() -> usize
    where
        Self: Sized;
    /// Fill `buf` with the current board (0=empty, 1=placed, -1=falling piece).
    fn sense(&self, buf: &mut [f64; BOARD_SIZE]);
    /// Apply `outputs` as actions and advance one game tick.
    /// Returns `true` when the game is over.
    fn tick(&mut self, outputs: &[f64]) -> bool;
    fn score(&self) -> f64;
}

// ---------------------------------------------------------------------------
// Generic Scenario wrapper
// ---------------------------------------------------------------------------

/// Wraps any [`TetrisEngine`] as an eevee `Scenario`.
///
/// Uses `PhantomData<fn(u16) -> E>` so the struct is always `Send + Sync`
/// regardless of whether `E` itself is, because the engine is only ever
/// constructed locally inside `eval()` — never shared across threads.
pub struct TetrisScenario<E: TetrisEngine> {
    pub seed: u16,
    pub level: u8,
    _engine: PhantomData<fn(u16) -> E>,
}

impl<E: TetrisEngine> TetrisScenario<E> {
    pub fn new(seed: u16, level: u8) -> Self {
        Self { seed, level, _engine: PhantomData }
    }
}

impl<E, C, G, A> Scenario<C, G, A> for TetrisScenario<E>
where
    E: TetrisEngine,
    C: Connection,
    G: Genome<C> + ToNetwork<Continuous, C>,
    A: Fn(f64) -> f64,
{
    fn io(&self) -> (usize, usize) {
        (BOARD_SIZE, E::outputs())
    }

    fn eval(&self, genome: &G, σ: &A) -> f64 {
        let mut engine = E::new_game(self.seed, self.level);
        let mut network = genome.network();
        let mut sense = [0.0f64; BOARD_SIZE];
        loop {
            engine.sense(&mut sense);
            network.step(1, &sense, σ);
            if engine.tick(network.output()) {
                break;
            }
        }
        engine.score()
    }
}

// ---------------------------------------------------------------------------
// Shared watch state — updated by the hook, read by the display thread
// ---------------------------------------------------------------------------

static GENERATION: AtomicUsize = AtomicUsize::new(0);
static MAX_FITNESS: AtomicU64 = AtomicU64::new(0);

pub fn update_watch(generation: usize, max_fitness: f64) {
    GENERATION.store(generation, Ordering::Relaxed);
    MAX_FITNESS.store(max_fitness.to_bits(), Ordering::Relaxed);
}

pub fn read_watch() -> (usize, f64) {
    (
        GENERATION.load(Ordering::Relaxed),
        f64::from_bits(MAX_FITNESS.load(Ordering::Relaxed)),
    )
}

pub fn draw_footer(score: f64) {
    let (gen, _) = read_watch();
    let left = format!("{}", score as u64);
    let right = format!("{}", gen);
    let inner = (left.len() + 1 + right.len()).max(8);
    let spaces = inner - left.len() - right.len();
    println!("|{}|", "-".repeat(inner));
    println!("|{}{}{}|", left, " ".repeat(spaces), right);
}
