#![allow(mixed_script_confusables)]
#![allow(confusable_idents)]

pub mod scenarios;
pub mod tetris;

use clap::{Args, Parser};
use core::ops::ControlFlow;
use eevee::{
    activate::relu,
    genome::Genome,
    population::population_init,
    random::default_rng,
    scenario::{evolve, EvolutionHooks},
    serialize::{population_from_files, population_to_files},
    Connection, Scenario, SerializeFile,
};
use std::{
    fs::create_dir_all,
    sync::{Arc, Mutex},
};

pub use eevee::scenario::{Hook, Stats};

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
// CLI args — shared across all scenarios
// ---------------------------------------------------------------------------

#[derive(Args, Clone)]
pub struct CommonArgs {
    /// Stop after this generation
    #[arg(long, default_value_t = 400)]
    pub until_generation: usize,
    /// Stop when best fitness reaches this threshold
    #[arg(long)]
    pub until_fitness: Option<f64>,
    /// Print stats and save every N generations
    #[arg(long, default_value_t = 10)]
    pub report_every: usize,
    /// Population size (ignored when resuming from files)
    #[arg(long, default_value_t = 100)]
    pub population: usize,
    /// Show a live terminal display of the fittest genome playing
    #[arg(long)]
    pub watch: bool,
    /// Number of eval threads (1 to CPU count; parallel builds only)
    #[cfg(feature = "parallel")]
    #[arg(long)]
    pub thread_count: Option<usize>,
}

// ---------------------------------------------------------------------------
// Generic evolve runner
// ---------------------------------------------------------------------------

type WatchFn<G> = dyn Fn(&G) + Send + 'static;

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
            eprintln!("--thread-count must be between 1 and {max}");
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

    let runner_hook = move |stats: &mut Stats<C, G>| -> ControlFlow<()> {
        if watch {
            if let Some((genome, _)) = stats.species.first().and_then(|s| s.members.first()) {
                *hook_best.lock().unwrap() = Some(genome.clone());
            }
        }

        if !stats.generation.is_multiple_of(report_every) {
            return ControlFlow::Continue(());
        }

        let fittest = stats.fittest().unwrap();
        println!("gen {} best: {:.3}", stats.generation, fittest.1);
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
    );
}

// ---------------------------------------------------------------------------
// Unified CLI dispatcher
// ---------------------------------------------------------------------------

/// Dispatch to a named scenario package.
///
/// Parses common args (see [`CommonArgs`]) from argv.  Scenario-specific args
/// may follow `--` on the command line and are passed as a `Vec<String>` to
/// the chosen package function.
///
/// ```text
/// eevee-eval -p nes-tetris ./genomes --until-generation 200 -- --seed 42 --level 3
/// eevee-eval -l
/// ```
pub fn cli_run(packages: &[(&'static str, fn(&str, CommonArgs, Vec<String>))]) {
    #[derive(Parser)]
    #[command(name = "eevee-eval", about = "NEAT scenario runner")]
    struct CliArgs {
        /// List available scenario packages
        #[arg(short = 'l', long)]
        list: bool,
        /// Scenario package to run
        #[arg(short = 'p', long, value_name = "PKG")]
        package: Option<String>,
        /// Directory to load and save genomes
        dir: Option<String>,
        #[command(flatten)]
        common: CommonArgs,
    }

    // Split argv on `--` to separate common from scenario-specific args.
    let all: Vec<String> = std::env::args().collect();
    let sep = all.iter().position(|a| a == "--");
    let (head, extra) = match sep {
        Some(i) => (all[..i].to_vec(), all[i + 1..].to_vec()),
        None => (all, vec![]),
    };

    let args = CliArgs::parse_from(head);

    if args.list {
        for (name, _) in packages {
            println!("{}", name);
        }
        return;
    }

    match args.package {
        None => {
            use clap::CommandFactory;
            CliArgs::command().print_help().unwrap();
            println!();
        }
        Some(ref pkg) => match packages.iter().find(|(name, _)| *name == pkg) {
            None => {
                eprintln!("unknown package '{}'. Use -l to list.", pkg);
                std::process::exit(1);
            }
            Some((_, f)) => {
                let dir = args.dir.unwrap_or_else(|| {
                    eprintln!("error: <DIR> is required when using -p");
                    std::process::exit(1);
                });
                f(&dir, args.common, extra);
            }
        },
    }
}
