use clap::Parser;
use eevee_eval::CommonArgs;

#[derive(Parser)]
#[command(about = "Tetris NEAT evolver (pure-C engine)")]
struct Args {
    /// Directory to load and save genomes
    dir: String,
    #[command(flatten)]
    common: CommonArgs,
}

fn main() {
    // Args after `--` are passed to the scenario for seed/level/etc.
    let all: Vec<String> = std::env::args().collect();
    let sep = all.iter().position(|a| a == "--");
    let (head, extra) = match sep {
        Some(i) => (all[..i].to_vec(), all[i + 1..].to_vec()),
        None => (all, vec![]),
    };
    let args = Args::parse_from(head);
    eevee_eval::scenarios::c::run(&args.dir, args.common, extra);
}
