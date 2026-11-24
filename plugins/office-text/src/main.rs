fn main() {
    let mut plugin = lookout_office_text::handler::Helper;
    lookout_core::plugins::sdk::serve(&mut plugin);
}
