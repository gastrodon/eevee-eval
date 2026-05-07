fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: nes-tetris <config.yaml>");
        std::process::exit(1);
    });
    let config = eevee_eval::load_config(&path);
    eevee_eval::scenarios::nes::run(&config.dir, config.common, config.extra_vec());
}
