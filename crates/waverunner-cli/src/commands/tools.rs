//! `waverunner tools` — show external tool status and install hints.

use anyhow::Result;
use clap::Args;

#[derive(Args)]
pub struct ToolsArgs {
    /// Show tools in JSON format
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: ToolsArgs) -> Result<()> {
    let tools = wavecore::dsp::decoders::tools::detect_tools();

    if args.json {
        let json_tools: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "commands": t.commands,
                    "resolved_command": t.resolved_command,
                    "installed": t.installed,
                    "version": t.version.as_deref(),
                    "install_hint": t.install_hint,
                    "description": t.description,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_tools)?);
        return Ok(());
    }

    println!("External Tool Status:");
    println!();
    print!("{}", wavecore::dsp::decoders::tools::format_tool_status());

    Ok(())
}
