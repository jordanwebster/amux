mod client;
mod executor;
mod parser;
mod terminal;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use executor::{Executor, ExecutorConfig};

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn cargo_target_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_TARGET_DIR") {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            workspace_root().join(path)
        }
    } else {
        workspace_root().join("target")
    }
}

fn debug_binary_path(name: &str, target_dir: &Path) -> PathBuf {
    target_dir
        .join("debug")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

#[cfg(unix)]
fn make_binary_path_absolute(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

#[cfg(windows)]
fn make_binary_path_absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

#[derive(Parser)]
#[command(name = "e2e-runner")]
#[command(about = "E2E test runner for amux")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Exercise the public API as an independent gRPC client
    Client {
        #[command(subcommand)]
        command: client::ClientCommand,
    },
    /// Run E2E tests
    Run {
        /// Filter tests by name (substring match)
        #[arg(default_value = "")]
        filter: String,

        /// Path to test files directory
        #[arg(long, default_value = "e2e-tests")]
        test_dir: PathBuf,

        /// Path to amux binary
        #[arg(long)]
        amux_binary: Option<PathBuf>,

        /// Path to test-agent binary
        #[arg(long)]
        test_agent_binary: Option<PathBuf>,
    },
    /// Update expected outputs in test files
    Update {
        /// Filter tests by name (substring match)
        #[arg(default_value = "")]
        filter: String,

        /// Path to test files directory
        #[arg(long, default_value = "e2e-tests")]
        test_dir: PathBuf,
    },
}

fn find_test_files(test_dir: &Path, filter: &str) -> Vec<PathBuf> {
    let pattern = test_dir.join("*.test");
    let pattern_str = pattern.to_string_lossy();

    glob::glob(&pattern_str)
        .unwrap_or_else(|e| {
            eprintln!("Failed to read glob pattern: {}", e);
            std::process::exit(1);
        })
        .filter_map(Result::ok)
        .filter(|path| {
            if filter.is_empty() {
                true
            } else {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.contains(filter))
                    .unwrap_or(false)
            }
        })
        .collect()
}

fn build_default_binaries(build_amux: bool, build_test_agent: bool) {
    if !build_amux && !build_test_agent {
        return;
    }
    eprintln!("Building default e2e binaries through wt...");
    let status = std::process::Command::new("timeout")
        .args(["900", "wt", "build"])
        .current_dir(workspace_root())
        .status()
        .unwrap_or_else(|error| {
            eprintln!("failed to run wt build for e2e binaries: {error}");
            std::process::exit(1);
        });
    if !status.success() {
        eprintln!("wt build for e2e binaries failed");
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn run_tests(
    test_dir: PathBuf,
    filter: String,
    amux_binary: Option<PathBuf>,
    test_agent_binary: Option<PathBuf>,
) {
    let test_files = find_test_files(&test_dir, &filter);

    if test_files.is_empty() {
        if filter.is_empty() {
            eprintln!("No test files found in {}", test_dir.display());
        } else {
            eprintln!(
                "No test files matching '{}' found in {}",
                filter,
                test_dir.display()
            );
        }
        std::process::exit(1);
    }

    let build_amux = amux_binary.is_none();
    let build_test_agent = test_agent_binary.is_none();
    let target_dir = cargo_target_dir();
    build_default_binaries(build_amux, build_test_agent);

    // Find binaries and convert to absolute paths
    let amux_binary = make_binary_path_absolute(
        amux_binary.unwrap_or_else(|| debug_binary_path("amux", &target_dir)),
    );

    let test_agent_binary = make_binary_path_absolute(
        test_agent_binary.unwrap_or_else(|| debug_binary_path("test-agent", &target_dir)),
    );

    // Check binaries exist
    if !amux_binary.exists() && amux_binary != Path::new("amux") {
        eprintln!(
            "amux binary not found at {}. Did you run 'wt build'?",
            amux_binary.display()
        );
        std::process::exit(1);
    }

    if !test_agent_binary.exists() && test_agent_binary != Path::new("test-agent") {
        eprintln!(
            "test-agent binary not found at {}. Did you run 'wt build'?",
            test_agent_binary.display()
        );
        std::process::exit(1);
    }

    let config = ExecutorConfig {
        amux_binary,
        test_agent_binary,
        ..Default::default()
    };

    let executor = Executor::new(config);

    // Warm the OS page cache so the first real test doesn't pay a cold-start penalty.
    let _ = std::process::Command::new(&executor.config.amux_binary)
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::process::Command::new(&executor.config.test_agent_binary)
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let mut passed = 0;
    let mut failed = 0;

    println!("Running {} test(s)...\n", test_files.len());

    for test_file in &test_files {
        let test_name = test_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        print!("  {} ... ", test_name);

        match parser::parse_test_file(test_file) {
            Ok(test_case) => {
                let result = executor.run_test(&test_case);
                if result.passed {
                    println!("ok");
                    passed += 1;
                } else {
                    println!("FAILED");
                    if let Some(ref error) = result.error {
                        println!("    Error: {}", error);
                    }
                    failed += 1;
                }
            }
            Err(e) => {
                println!("FAILED (parse error)");
                println!("    {}", e);
                failed += 1;
            }
        }
    }

    println!();
    println!("Results: {} passed, {} failed", passed, failed);

    if failed > 0 {
        std::process::exit(1);
    }
}

fn update_tests(test_dir: PathBuf, filter: String) {
    // TODO: Implement update mode
    // This would run tests and capture actual output,
    // then rewrite the test files with new expected values
    eprintln!("Update mode not yet implemented");
    eprintln!(
        "Would update tests in {} matching '{}'",
        test_dir.display(),
        filter
    );
    std::process::exit(1);
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Client { command } => {
            let runtime = tokio::runtime::Runtime::new().expect("create client runtime");
            if let Err(error) = runtime.block_on(client::run(command)) {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        Commands::Run {
            filter,
            test_dir,
            amux_binary,
            test_agent_binary,
        } => {
            run_tests(test_dir, filter, amux_binary, test_agent_binary);
        }
        Commands::Update { filter, test_dir } => {
            update_tests(test_dir, filter);
        }
    }
}
