fn main() {
    let mut plugin = lookout_files_tools::handler::FileTools;
    lookout_core::plugins::sdk::serve(&mut plugin);
}
