use lookout_core::plugins::sdk::serve;
use lookout_screenshot::editor::run_real_window_4k_benchmark;
use lookout_screenshot::handler::ScreenshotPlugin;

fn main() {
    if std::env::var("LOOKOUT_MEASURE_4K").is_ok() {
        let (avg, min, max, count) = run_real_window_4k_benchmark(60);
        println!(
            "REAL_WINDOW_4K_BENCHMARK: avg={:.3} ms, min={:.3} ms, max={:.3} ms, frames={}",
            avg, min, max, count
        );
        return;
    }

    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1].as_str() == "--measure-4k-window" {
        let frames = if args.len() > 2 {
            args[2].parse::<u32>().unwrap_or(60)
        } else {
            60
        };
        let (avg, min, max, count) = run_real_window_4k_benchmark(frames);
        println!(
            "REAL_WINDOW_4K_BENCHMARK: avg={:.3} ms, min={:.3} ms, max={:.3} ms, frames={}",
            avg, min, max, count
        );
        return;
    }

    let mut plugin = ScreenshotPlugin::new();
    serve(&mut plugin);
}
