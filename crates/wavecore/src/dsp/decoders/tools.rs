//! External tool detection and install prompts.
//!
//! Checks whether required external tools are installed, resolves compatible
//! command aliases where needed, and provides install hints when they're not.

use std::env;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy)]
struct ToolDef {
    name: &'static str,
    commands: &'static [&'static str],
    version_flags: &'static [&'static str],
    install_hint: &'static str,
    description: &'static str,
}

/// Information about an external tool that waverunner can use.
#[derive(Debug, Clone)]
pub struct ExternalTool {
    /// Human-readable canonical name.
    pub name: &'static str,
    /// Compatible command names checked on `$PATH`.
    pub commands: &'static [&'static str],
    /// Which command was actually found on `$PATH`.
    pub resolved_command: Option<&'static str>,
    /// Whether it was found on `$PATH`.
    pub installed: bool,
    /// Version string if detected.
    pub version: Option<String>,
    /// Suggested install hint(s).
    pub install_hint: &'static str,
    /// What protocols/features this tool provides.
    pub description: &'static str,
}

/// All external tools that waverunner integrates with.
const TOOL_DEFS: &[ToolDef] = &[
    ToolDef {
        name: "rtl_433",
        commands: &["rtl_433"],
        version_flags: &["-V", "--version"],
        install_hint: "Arch: sudo pacman -S rtl_433 | Debian/Ubuntu: sudo apt install rtl-433 | macOS: brew install rtl_433",
        description: "433/315/868/915 MHz ISM band sensors (250+ device types)",
    },
    ToolDef {
        name: "redsea",
        commands: &["redsea"],
        version_flags: &["--version", "-h"],
        install_hint: "Arch (AUR): paru -S redsea | Other distros: install from packages if available, otherwise build from source",
        description: "FM broadcast RDS/RBDS decoding",
    },
    ToolDef {
        name: "multimon-ng",
        commands: &["multimon-ng"],
        version_flags: &["-h", "--help"],
        install_hint: "Arch: sudo pacman -S multimon-ng | Debian/Ubuntu: sudo apt install multimon-ng | macOS: brew install multimon-ng",
        description: "POCSAG, APRS, DTMF, EAS, FLEX decoding",
    },
    ToolDef {
        name: "dump1090",
        commands: &["dump1090", "dump1090-fa", "readsb"],
        version_flags: &["--version", "--help"],
        install_hint: "Arch (AUR): paru -S dump1090-fa-git or readsb-git | Install another stdin-compatible dump1090 backend on other distros; dump1090_rs is not currently supported",
        description: "ADS-B / Mode-S aircraft decoding (dump1090, dump1090-fa, or readsb)",
    },
];

fn tool_def(name: &str) -> Option<&'static ToolDef> {
    TOOL_DEFS
        .iter()
        .find(|def| def.name == name || def.commands.contains(&name))
}

fn command_candidates(command: &str) -> Vec<OsString> {
    let mut candidates = vec![OsString::from(command)];

    if cfg!(windows) && Path::new(command).extension().is_none() {
        let pathext =
            env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
        for ext in pathext
            .to_string_lossy()
            .split(';')
            .filter(|ext| !ext.is_empty())
        {
            let mut candidate = OsString::from(command);
            candidate.push(ext);
            candidates.push(candidate);
        }
    }

    candidates
}

fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute()
        || command.contains(std::path::MAIN_SEPARATOR)
        || command.contains('/')
        || command.contains('\\')
    {
        return path.is_file();
    }

    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };

    for dir in env::split_paths(&path_var) {
        for candidate in command_candidates(command) {
            if dir.join(candidate).is_file() {
                return true;
            }
        }
    }

    false
}

fn first_available_command(commands: &'static [&'static str]) -> Option<&'static str> {
    commands
        .iter()
        .copied()
        .find(|command| command_exists(command))
}

/// Check if a command is available on `$PATH`.
pub fn is_available(command: &str) -> bool {
    command_exists(command)
}

/// Get the install hint for a command.
pub fn install_hint(command: &str) -> &'static str {
    tool_def(command)
        .map(|def| def.install_hint)
        .unwrap_or("Check your package manager")
}

/// Get the description for a command.
pub fn tool_description(command: &str) -> &'static str {
    tool_def(command).map(|def| def.description).unwrap_or("")
}

/// Get compatible command aliases for a canonical tool.
pub fn tool_commands(command: &str) -> &'static [&'static str] {
    tool_def(command).map(|def| def.commands).unwrap_or(&[])
}

/// Resolve the actual command available on `$PATH` for a canonical tool.
pub fn resolve_tool_command(command: &str) -> Option<&'static str> {
    let canonical = tool_def(command).map(|def| def.name)?;
    cached_tools()
        .iter()
        .find(|tool| tool.name == canonical)
        .and_then(|tool| tool.resolved_command)
}

fn extract_version(output: &str) -> Option<String> {
    for line in output.lines().take(8) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        for word in line.split_whitespace() {
            let clean = word.trim_start_matches('v').trim_end_matches(',');
            if clean.contains('.') && clean.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return Some(clean.to_string());
            }
        }
    }
    None
}

/// Try to get the version string from a tool.
fn get_version(command: &str, flags: &[&str]) -> Option<String> {
    for &flag in flags {
        let output = Command::new(command)
            .arg(flag)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .ok()?;

        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        if let Some(version) = extract_version(&combined) {
            return Some(version);
        }
    }

    None
}

/// Detect all known external tools and their status.
pub fn detect_tools() -> Vec<ExternalTool> {
    TOOL_DEFS
        .iter()
        .map(|def| {
            let resolved_command = first_available_command(def.commands);
            let installed = resolved_command.is_some();
            let version =
                resolved_command.and_then(|command| get_version(command, def.version_flags));
            ExternalTool {
                name: def.name,
                commands: def.commands,
                resolved_command,
                installed,
                version,
                install_hint: def.install_hint,
                description: def.description,
            }
        })
        .collect()
}

/// Cached tool availability (computed once per process).
static TOOL_CACHE: OnceLock<Vec<ExternalTool>> = OnceLock::new();

/// Get cached tool detection results.
pub fn cached_tools() -> &'static Vec<ExternalTool> {
    TOOL_CACHE.get_or_init(detect_tools)
}

/// Check if a specific tool is available (cached).
pub fn is_tool_available(command: &str) -> bool {
    resolve_tool_command(command).is_some()
}

/// Format all tool statuses for display.
pub fn format_tool_status() -> String {
    let tools = detect_tools();
    let mut lines = Vec::new();
    let mut available = 0;

    for tool in &tools {
        if tool.installed {
            available += 1;
            let ver = tool.version.as_deref().unwrap_or("?");
            let resolved = match tool.resolved_command {
                Some(command) if command != tool.name => format!(" via {command}"),
                _ => String::new(),
            };
            lines.push(format!(
                "  {:<14} v{:<8} ✓  {}{}",
                tool.name, ver, tool.description, resolved
            ));
        } else {
            lines.push(format!(
                "  {:<14} {:>9} ✗  {}",
                tool.name, "", tool.description
            ));
            lines.push(format!(
                "  {:>14}           commands: {}",
                "",
                tool.commands.join(", ")
            ));
            lines.push(format!("  {:>14}           → {}", "", tool.install_hint));
        }
    }

    let total = tools.len();
    lines.push(String::new());
    lines.push(format!("{available}/{total} tools installed"));

    let missing: Vec<&str> = tools
        .iter()
        .filter(|tool| !tool.installed)
        .map(|tool| tool.install_hint)
        .collect();
    if !missing.is_empty() {
        lines.push(format!("Install all: {}", missing.join(" && ")));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_tools_returns_all() {
        let tools = detect_tools();
        assert_eq!(tools.len(), TOOL_DEFS.len());
    }

    #[test]
    fn known_tool_commands_are_exposed() {
        assert_eq!(
            tool_commands("dump1090"),
            &["dump1090", "dump1090-fa", "readsb"]
        );
        assert_eq!(
            tool_commands("readsb"),
            &["dump1090", "dump1090-fa", "readsb"]
        );
    }

    #[test]
    fn install_hint_known() {
        let hint = install_hint("redsea");
        assert!(hint.contains("redsea"));
    }

    #[test]
    fn install_hint_unknown() {
        let hint = install_hint("nonexistent_tool");
        assert!(hint.contains("package manager"));
    }

    #[test]
    fn is_available_basic() {
        assert!(is_available("ls"));
        assert!(!is_available("definitely_not_a_real_command_xyz_12345"));
    }

    #[test]
    fn first_available_command_uses_second_alias() {
        assert_eq!(
            first_available_command(&["definitely_not_a_real_command_xyz_12345", "ls"]),
            Some("ls")
        );
    }

    #[test]
    fn format_status_nonempty() {
        let status = format_tool_status();
        assert!(!status.is_empty());
        assert!(status.contains("tools installed"));
    }
}
