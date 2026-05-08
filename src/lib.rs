#![allow(mixed_script_confusables)]
#![allow(confusable_idents)]

pub mod scenarios;
pub mod tetris;

use core::ops::ControlFlow;
use eevee::{
    activate::relu,
    genome::Genome,
    population::population_init,
    random::default_rng,
    scenario::{evolve, EvolutionConfig, EvolutionHooks},
    serialize::{population_from_files, population_to_files},
    Connection, Scenario, SerializeFile,
};
use serde::Deserialize;
use std::{
    fs::create_dir_all,
    sync::{Arc, Mutex, RwLock},
};

pub use eevee::scenario::{Hook, Stats};

pub fn report_generation(
    generation: usize,
    fittest: f64,
    species_sizes: &[usize],
    hall: Option<usize>,
    stale: Option<usize>,
) {
    let total: usize = species_sizes.iter().sum();
    let shares: Vec<String> = species_sizes
        .iter()
        .filter_map(|&n| (n != 0).then_some(format!("{}%", (n * 100) / total.max(1))))
        .collect();
    let hall_str = hall
        .map(|h| {
            format!(
                "  hall {}/{}",
                h,
                crate::scenarios::board_game::HALL_OF_FAME_MAX
            )
        })
        .unwrap_or_default();
    let stale_str = stale
        .filter(|&s| s > 0)
        .map(|s| format!("  stale: {}", s))
        .unwrap_or_default();
    eprintln!(
        "gen {} best: {:.3}  species: {}  [{}]{}{}",
        generation,
        fittest,
        species_sizes.len(),
        shares.join(", "),
        hall_str,
        stale_str
    );
}

// ---------------------------------------------------------------------------
// Terminal rendering
// ---------------------------------------------------------------------------

const BLOCKS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Render a flat slice of values into a colored block-character grid.
/// Values in [-1, 0): white fg; values in [0, 1]: black fg. Gray background.
/// When `spacing` is true a blank column is inserted between each cell.
pub fn render_frame(values: &[f64], width: usize, spacing: bool) -> String {
    let mut out = String::new();
    for (i, &v) in values.iter().enumerate() {
        let col = i % width;
        if i > 0 && col == 0 {
            out.push_str("\x1b[0m\n");
        }
        if spacing && col > 0 {
            out.push_str("\x1b[0m ");
        }
        let magnitude = v.abs().clamp(0.0, 1.0);
        let block_idx = (magnitude * 7.0).round() as usize;
        let block = BLOCKS[block_idx];
        let fg = if v < 0.0 { "\x1b[97m" } else { "\x1b[30m" };
        out.push_str(&format!("\x1b[100m{}{}", fg, block));
    }
    out.push_str("\x1b[0m\n");
    out
}

/// Render a row of output values with labeled columns below.
/// Spacing is always enabled so labels align 1:1 with blocks.
pub fn draw_output(outputs: &[f64], labels: &[char]) {
    print!("{}", render_frame(outputs, outputs.len(), true));
    let label_row: String = labels
        .iter()
        .enumerate()
        .flat_map(|(i, &c)| if i > 0 { vec![' ', c] } else { vec![c] })
        .collect();
    println!("{}", label_row);
}

// ---------------------------------------------------------------------------
// Shared config (deserialised from YAML)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct CommonArgs {
    pub until_generation: usize,
    pub until_fitness: Option<f64>,
    pub report_every: usize,
    pub population: usize,
    pub watch: bool,
    #[cfg(feature = "parallel")]
    pub thread_count: Option<usize>,
    pub specie_threshold: f64,
    pub no_improvement_truncate: usize,
    pub no_improvement_floor: usize,
    pub copy_denom: usize,
    pub specie_youth_fac: f64,
    pub specie_youth_dropoff: usize,
    pub specie_min_pop: usize,
}

impl Default for CommonArgs {
    fn default() -> Self {
        Self {
            until_generation: 400,
            until_fitness: None,
            report_every: 10,
            population: 100,
            watch: false,
            #[cfg(feature = "parallel")]
            thread_count: None,
            specie_threshold: 4.0,
            no_improvement_truncate: 10,
            no_improvement_floor: 2,
            copy_denom: 4,
            specie_youth_fac: 2.0,
            specie_youth_dropoff: 10,
            specie_min_pop: 2,
        }
    }
}

impl CommonArgs {
    pub fn evolution_config(&self) -> EvolutionConfig {
        EvolutionConfig {
            specie_threshold: self.specie_threshold,
            no_improvement_truncate: self.no_improvement_truncate,
            no_improvement_floor: self.no_improvement_floor,
            copy_denom: self.copy_denom,
            specie_youth_fac: self.specie_youth_fac,
            specie_youth_dropoff: self.specie_youth_dropoff,
            specie_min_pop: self.specie_min_pop,
        }
    }
}

/// Top-level YAML config understood by [`cli_run`] and [`load_config`].
///
/// ```yaml
/// package: nes-mario
/// dir: ./genomes/mario
/// extra: "--seed 0 --level 3"
/// until_generation: 400
/// population: 150
/// ```
///
/// All fields except `package` and `dir` are optional and default to the
/// values documented on [`CommonArgs`].
#[derive(Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub package: String,
    pub dir: String,
    pub extra: String,
    #[serde(flatten)]
    pub common: CommonArgs,
}

impl Config {
    pub fn extra_vec(&self) -> Vec<String> {
        self.extra
            .split_whitespace()
            .map(|s| s.to_owned())
            .collect()
    }
}

pub fn load_config(path: &str) -> Config {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("error reading '{}': {}", path, e);
        std::process::exit(1);
    });
    serde_yaml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("error parsing '{}': {}", path, e);
        std::process::exit(1);
    })
}

pub type WatchFn<G> = dyn Fn(&G) + Send + 'static;
pub type Pool<G> = Arc<RwLock<Vec<G>>>;

pub fn run<
    C: Connection,
    G: Genome<C> + SerializeFile + Clone + Send + 'static,
    #[cfg(not(feature = "parallel"))] S: Scenario<C, G, fn(f64) -> f64>,
    #[cfg(feature = "parallel")] S: Scenario<C, G, fn(f64) -> f64> + Sync,
>(
    scenario: S,
    dir: &str,
    args: CommonArgs,
    watch_fn: Option<Box<WatchFn<G>>>,
    extra_hooks: Vec<Hook<C, G>>,
) {
    #[cfg(feature = "parallel")]
    {
        let max = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let n = args.thread_count.unwrap_or(max);
        if n < 1 || n > max {
            eprintln!("thread_count must be between 1 and {max}");
            std::process::exit(1);
        }
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
            .unwrap();
    }

    create_dir_all(dir).expect("failed to create genome output directory");

    let init = population_from_files(dir).unwrap_or_else(|_| {
        let (inputs, outputs) = scenario.io();
        population_init::<C, G>(inputs, outputs, args.population)
    });

    let best: Arc<Mutex<Option<G>>> = {
        let seed = init
            .0
            .first()
            .and_then(|s| s.members.first())
            .map(|(g, _)| g.clone());
        Arc::new(Mutex::new(seed))
    };

    if args.watch {
        if let Some(f) = watch_fn {
            let slot = Arc::clone(&best);
            std::thread::spawn(move || {
                print!("\x1b[2J\x1b[H");
                loop {
                    let genome = slot.lock().unwrap().clone();
                    if let Some(g) = genome {
                        f(&g)
                    }
                }
            });
        }
    }

    let hook_best = Arc::clone(&best);
    let watch = args.watch;
    let dir = dir.to_owned();
    let report_every = args.report_every;
    let until_generation = args.until_generation;
    let until_fitness = args.until_fitness;
    let config = args.evolution_config();

    let runner_hook = move |stats: &mut Stats<C, G>| -> ControlFlow<()> {
        if watch {
            if let Some((genome, _)) = stats.species.first().and_then(|s| s.members.first()) {
                *hook_best.lock().unwrap() = Some(genome.clone());
            }
        }

        if !stats.generation.is_multiple_of(report_every) {
            return ControlFlow::Continue(());
        }

        let Some(fittest) = stats.fittest() else {
            return ControlFlow::Continue(());
        };
        let sizes: Vec<usize> = stats.species.iter().map(|s| s.members.len()).collect();
        crate::report_generation(stats.generation, fittest.1, &sizes, None, None);
        population_to_files(&dir, stats.species).unwrap();

        if stats.generation >= until_generation {
            return ControlFlow::Break(());
        }
        if let Some(threshold) = until_fitness {
            if fittest.1 >= threshold {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    };

    let mut hooks = extra_hooks;
    hooks.push(Box::new(runner_hook));

    evolve(
        scenario,
        |_| init,
        relu as fn(f64) -> f64,
        default_rng(),
        EvolutionHooks::new(hooks),
        config,
    );
}

type EvalRunnable = (&'static str, fn(&str, CommonArgs, Vec<String>));

/// Dispatch to a named scenario package from a YAML config file.
///
/// ```text
/// eevee-eval run.yaml
/// eevee-eval -l
/// ```
pub fn cli_run(packages: &[EvalRunnable]) {
    match std::env::args().nth(1).as_deref() {
        None | Some("-l") | Some("--list") => {
            for (name, _) in packages {
                println!("{}", name);
            }
        }
        Some(path) => {
            let config = load_config(path);
            match packages.iter().find(|(name, _)| *name == config.package) {
                None => {
                    eprintln!("unknown package '{}'. Use -l to list.", config.package);
                    std::process::exit(1);
                }
                Some((_, f)) => {
                    let extra = config.extra_vec();
                    f(&config.dir, config.common, extra)
                }
            }
        }
    }
}
