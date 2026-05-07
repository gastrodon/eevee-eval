fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: tetris-c <config.yaml>");
        std::process::exit(1);
    });
    let config = eevee_eval::load_config(&path);
    let common = config.extra_vec();
    eevee_eval::scenarios::c::run(&config.dir, config.common, common);
}
