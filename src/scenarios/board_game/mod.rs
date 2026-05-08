pub mod ataxx;
pub mod connect4;
pub mod oware;
pub mod ttt;

use crate::{CommonArgs, Hook, Pool, Stats, WatchFn};
use board_game::board::Player;
use core::ops::ControlFlow;
use eevee::{
    genome::{Recurrent, WConnection},
    network::{activate::steep_sigmoid, FromGenome, Network, ToNetwork},
    population::population_init,
    random::{seed_urandom, WyRng},
    scenario::{evolve, EvolutionHooks},
    serialize::{population_from_files, population_to_files},
    Scenario,
};
use rand::Rng;
use std::{
    fs::create_dir_all,
    marker::PhantomData,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

pub type C = WConnection;
pub type G = Recurrent<C>;

// ---------------------------------------------------------------------------
// Co-evolution scenario trait
// ---------------------------------------------------------------------------

/// Game-specific logic for a co-evolutionary board-game scenario.
///
/// Implement this on a unit struct; `CoEvolScenario<T, NN>` provides the full
/// `Scenario` impl (pool sampling, alternating sides, averaging) for free.
pub trait CoEvolGame {
    const GAMES_PER_EVAL: usize;
    fn io() -> (usize, usize);
    fn play<NN: Network, A: Fn(f64) -> f64>(
        learner: &mut NN,
        learner_player: Player,
        opponent: Option<&mut NN>,
        σ: &A,
        rng: &mut WyRng,
    ) -> f64;
}

pub struct CoEvolScenario<T, NN> {
    pub pool: Pool<G>,
    seed_counter: AtomicU64,
    _game: PhantomData<fn() -> T>,
    _network: PhantomData<fn() -> NN>,
}

impl<T: CoEvolGame, NN> CoEvolScenario<T, NN> {
    pub fn new(pool: Pool<G>, base_seed: u64) -> Self {
        Self {
            pool,
            seed_counter: AtomicU64::new(base_seed),
            _game: PhantomData,
            _network: PhantomData,
        }
    }
}

impl<T: CoEvolGame, NN: Network + FromGenome<C, G>, A: Fn(f64) -> f64> Scenario<C, G, A>
    for CoEvolScenario<T, NN>
{
    fn io(&self) -> (usize, usize) {
        T::io()
    }

    fn eval(&self, genome: &G, σ: &A) -> f64 {
        let seed = self.seed_counter.fetch_add(1, Ordering::Relaxed);
        let mut rng = WyRng::seeded(seed);

        let opponents: Vec<G> = {
            let pool = self.pool.read().unwrap();
            if pool.is_empty() {
                vec![]
            } else {
                (0..T::GAMES_PER_EVAL)
                    .map(|_| pool[rng.random_range(0..pool.len())].clone())
                    .collect()
            }
        };

        let mut learner: NN = genome.network();
        let mut total = 0.0;

        for i in 0..T::GAMES_PER_EVAL {
            let learner_player = if i % 2 == 0 { Player::A } else { Player::B };
            let score = if opponents.is_empty() {
                T::play(&mut learner, learner_player, None, σ, &mut rng)
            } else {
                let mut opp: NN = opponents[i % opponents.len()].network();
                T::play(&mut learner, learner_player, Some(&mut opp), σ, &mut rng)
            };
            total += score;
        }

        total / T::GAMES_PER_EVAL as f64
    }
}

// ---------------------------------------------------------------------------
// Hall-of-fame management hook
// ---------------------------------------------------------------------------

pub const HALL_OF_FAME_MAX: usize = 64;

pub fn refresh_hook(pool: Pool<G>) -> Hook<C, G> {
    Box::new(move |stats| {
        if let Some((g, _)) = stats.fittest() {
            let champ = g.clone();
            let mut pool = pool.write().unwrap();
            pool.push(champ);
            let drop_n = pool.len().saturating_sub(HALL_OF_FAME_MAX);
            if drop_n > 0 {
                pool.drain(0..drop_n);
            }
        }
        ControlFlow::Continue(())
    })
}

// ---------------------------------------------------------------------------
// Common board-game runner
// ---------------------------------------------------------------------------

/// Shared boilerplate for all co-evolutionary board-game scenarios.
///
/// Handles population init / resume, file saving, reporting, and termination.
/// Each game creates its scenario + pool and calls this.
pub fn board_game_run<
    #[cfg(not(feature = "parallel"))] S: Scenario<C, G, fn(f64) -> f64>,
    #[cfg(feature = "parallel")] S: Scenario<C, G, fn(f64) -> f64> + Sync,
>(
    scenario: S,
    pool: Pool<G>,
    dir: &str,
    common: CommonArgs,
    watch_fn: Option<Box<WatchFn<G>>>,
) {
    create_dir_all(dir).expect("failed to create genome output directory");

    let (inputs, outputs) = scenario.io();
    let init = population_from_files(dir)
        .unwrap_or_else(|_| population_init::<C, G>(inputs, outputs, common.population));

    let watch = common.watch;
    let until_generation = common.until_generation;
    let until_fitness = common.until_fitness;
    let report_every = common.report_every;
    let config = common.evolution_config();
    let dir_owned = dir.to_owned();
    let pool_for_save = Arc::clone(&pool);

    let best: Arc<Mutex<Option<G>>> = Arc::new(Mutex::new(None));

    if watch {
        if let Some(f) = watch_fn {
            let slot = Arc::clone(&best);
            std::thread::spawn(move || {
                print!("\x1b[2J\x1b[H");
                loop {
                    let genome = slot.lock().unwrap().clone();
                    if let Some(g) = genome {
                        f(&g);
                    }
                }
            });
        }
    }

    let hook_best = Arc::clone(&best);
    let last_best = AtomicU64::new(f64::NEG_INFINITY.to_bits());
    let stale = AtomicUsize::new(0);
    let save_hook: Hook<C, G> = Box::new(move |stats: &mut Stats<'_, C, G>| {
        if watch {
            if let Some((genome, _)) = stats.species.first().and_then(|s| s.members.first()) {
                *hook_best.lock().unwrap() = Some(genome.clone());
            }
        }
        if let Some((_, f)) = stats.fittest() {
            let prev = f64::from_bits(last_best.load(Ordering::Relaxed));
            if *f > prev {
                last_best.store(f.to_bits(), Ordering::Relaxed);
                stale.store(0, Ordering::Relaxed);
            } else {
                stale.fetch_add(1, Ordering::Relaxed);
            }
        }
        if stats.generation.is_multiple_of(report_every) {
            if let Some((_, f)) = stats.fittest() {
                let hall_size = pool_for_save.read().unwrap().len();
                let sizes: Vec<usize> = stats.species.iter().map(|s| s.members.len()).collect();
                crate::report_generation(
                    stats.generation,
                    *f,
                    &sizes,
                    Some(hall_size),
                    Some(stale.load(Ordering::Relaxed)),
                );
                population_to_files(&dir_owned, stats.species).unwrap();
            }
        }
        if stats.generation >= until_generation {
            return ControlFlow::Break(());
        }
        if let Some(threshold) = until_fitness {
            if stats.fittest().map(|(_, f)| *f).unwrap_or(0.0) >= threshold {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    });

    let base_seed = seed_urandom().unwrap();

    evolve(
        scenario,
        |_| init,
        steep_sigmoid as fn(f64) -> f64,
        WyRng::seeded(base_seed),
        EvolutionHooks::new(vec![refresh_hook(pool), save_hook]),
        config,
    );
}
