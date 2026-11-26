mod cli;
mod core;
mod mcp;
mod tools;

const USAGE: &str = "agyfast-rs <mcp | hook session-start | hook pre-bash | cli <search|outline|slices|edit|check|filter|snapshot> ...>
  mcp                  stdio MCP server (JSON-RPC on stdout, logs on stderr)
  hook session-start   print the workspace snapshot for a SessionStart hook
  hook pre-bash        Claude Code PreToolUse hook: pipe build/test/lint Bash commands through `cli filter`
  cli --help           one-shot tool calls for shells and swarm workers";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["mcp"] => mcp::serve(),
        ["hook", "session-start", ..] => {
            tools::snapshot::hook();
            0
        }
        ["hook", "pre-bash", ..] => {
            tools::check::pre_bash_hook();
            0
        }
        ["cli", ..] => cli::run(&args[1..]),
        ["--version" | "-V"] => {
            println!("agyfast-rs {}", env!("CARGO_PKG_VERSION"));
            0
        }
        ["--help" | "-h"] => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}
