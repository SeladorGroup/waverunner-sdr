use anyhow::Result;

#[derive(clap::Args)]
pub struct ProbeArgs {
    /// Output as JSON instead of human-readable text
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: ProbeArgs) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    // Check rtl_433 availability
    let rtl433_version = match std::process::Command::new("rtl_433")
        .arg("-V")
        .output()
    {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let combined = format!("{stdout}{stderr}");
            combined
                .lines()
                .find(|l| l.contains("rtl_433"))
                .unwrap_or("found (unknown version)")
                .trim()
                .to_string()
        }
        Err(_) => "not found".to_string(),
    };

    // Report wavecore feature status via its re-exported markers
    let audio_info = if wavecore::hardware::has_audio_feature() {
        "cpal (compiled)"
    } else {
        "disabled"
    };

    let rtlsdr_info = if wavecore::hardware::has_rtlsdr_feature() {
        "enabled"
    } else {
        "disabled"
    };

    if args.json {
        let doc = serde_json::json!({
            "waverunner_version": version,
            "os": os,
            "arch": arch,
            "rust_edition": "2024",
            "rtlsdr_feature": rtlsdr_info,
            "audio_feature": audio_info,
            "rtl_433": rtl433_version,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        println!("WaveRunner Environment Probe");
        println!("============================");
        println!();
        println!("  Version:       {version}");
        println!("  OS:            {os}");
        println!("  Architecture:  {arch}");
        println!("  Rust edition:  2024");
        println!();
        println!("Features:");
        println!("  RTL-SDR:       {rtlsdr_info}");
        println!("  Audio:         {audio_info}");
        println!();
        println!("External tools:");
        println!("  rtl_433:       {rtl433_version}");
    }

    Ok(())
}
