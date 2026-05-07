pub mod ataxx;
pub mod connect4;
pub mod oware;
pub mod ttt;

use crate::{CommonArgs, Hook, Stats};
use core::ops::ControlFlow;
use eevee::{
    genome::{Recurrent, WConnection},
    network::activate::steep_sigmoid,
    population::population_init,
    random::{seed_urandom, WyRng},
    scenario::{evolve, EvolutionHooks},
    serialize::{population_from_files, population_to_files},
    Scenario,
};
use std::{
    fs::create_dir_all,
    sync::{Arc, RwLock},
};

pub type C = WConnection;
pub type G = Recurrent<C>;

// ---------------------------------------------------------------------------
// Hall-of-fame management hook
// ---------------------------------------------------------------------------

const HALL_OF_FAME_MAX: usize = 64;

pub fn refresh_hook(
    pool: Arc<RwLock<Vec<G>>>,
) -> Box<dyn Fn(&mut Stats<'_, C, G>) -> ControlFlow<()>> {
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
    #[cfg(not(feature = "parallel"))]
    S: Scenario<C, G, fn(f64) -> f64>,
    #[cfg(feature = "parallel")]
    S: Scenario<C, G, fn(f64) -> f64> + Sync,
>(
    scenario: S,
    pool: Arc<RwLock<Vec<G>>>,
    dir: &str,
    common: CommonArgs,
) {
    create_dir_all(dir).expect("failed to create genome output directory");

    let (inputs, outputs) = scenario.io();
    let init = population_from_files(dir)
        .unwrap_or_else(|_| population_init::<C, G>(inputs, outputs, common.population));

    let until_generation = common.until_generation;
    let until_fitness = common.until_fitness;
    let report_every = common.report_every;
    let dir_owned = dir.to_owned();
    let pool_for_save = Arc::clone(&pool);

    let save_hook: Hook<C, G> = Box::new(move |stats: &mut Stats<'_, C, G>| {
        if stats.generation % report_every == 0 {
            if let Some((_, f)) = stats.fittest() {
                let hall_size = pool_for_save.read().unwrap().len();
                println!(
                    "gen {} best: {:.4} | hall {}",
                    stats.generation, f, hall_size
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
    );
}
