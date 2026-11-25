//! `lookout` (built as lookout-cli.exe, staged as cli\lookout.exe).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(transport) = lookout_cli::winpipe::PipeTransport::default_names() else {
        eprintln!("lookout: could not read your Windows user id");
        return ExitCode::from(1);
    };
    let code = lookout_cli::cmd::run(
        &args,
        &transport,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
