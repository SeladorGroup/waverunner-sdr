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
    let tools = wavecore::dsp::decoders::tools::detect_tools();
    let rtl433_status = tools
        .iter()
        .find(|tool| tool.name == "rtl_433")
        .map(|tool| {
            if !tool.installed {
                "not found".to_string()
            } else {
                match (tool.version.as_deref(), tool.resolved_command) {
                    (Some(version), Some(command)) if command != tool.name => {
                        format!("{version} via {command}")
                    }
                    (Some(version), _) => version.to_string(),
                    (None, Some(command)) => format!("found via {command}"),
                    (None, None) => "found".to_string(),
                }
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

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
        let external_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "commands": tool.commands,
                    "resolved_command": tool.resolved_command,
                    "installed": tool.installed,
                    "version": tool.version.as_deref(),
                    "description": tool.description,
                })
            })
            .collect();
        let doc = serde_json::json!({
            "waverunner_version": version,
            "os": os,
            "arch": arch,
            "rust_edition": "2024",
            "rtlsdr_feature": rtlsdr_info,
            "audio_feature": audio_info,
            "rtl_433": rtl433_status,
            "external_tools": external_tools,
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
        for tool in &tools {
            let status = if tool.installed {
                match (tool.version.as_deref(), tool.resolved_command) {
                    (Some(version), Some(command)) if command != tool.name => {
                        format!("v{version} via {command}")
                    }
                    (Some(version), _) => format!("v{version}"),
                    (None, Some(command)) => format!("found via {command}"),
                    (None, None) => "found".to_string(),
                }
            } else {
                "missing".to_string()
            };
            println!("  {:<14} {}", tool.name, status);
        }
    }

    Ok(())
}
