fn main() {
    let mut plugin = lookout_semantic::handler::Semantic::new(Box::new(
        lookout_semantic::embed::load_from_assets,
    ));
    lookout_core::plugins::sdk::serve(&mut plugin);
}
