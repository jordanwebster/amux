use serde::Deserialize;
use std::path::Path;

/// Directory definition in test environment
#[derive(Debug, Clone, Deserialize)]
pub struct Directory {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// Config definition in test environment
#[derive(Debug, Clone, Deserialize)]
pub struct TestConfig {
    pub name: String,
    #[serde(default)]
    pub host_id: Option<String>,
    #[serde(default)]
    pub socket_path: Option<String>,
    #[serde(default)]
    pub tcp_port: Option<u16>,
}

/// Terminal definition in test environment
#[derive(Debug, Clone, Deserialize)]
pub struct Terminal {
    pub name: String,
    #[serde(default)]
    pub config: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

/// A single step in a test
#[derive(Debug, Clone)]
pub enum TestStep {
    /// Switch to a different terminal
    SwitchTerminal(String),
    /// Send input (a line to type)
    Input(String),
    /// Expect output (may be multiple lines, joined with \n)
    ExpectOutput(String),
}

/// A parsed test case
#[derive(Debug)]
pub struct TestCase {
    pub name: String,
    pub directories: Vec<Directory>,
    pub configs: Vec<TestConfig>,
    pub terminals: Vec<Terminal>,
    pub steps: Vec<TestStep>,
}

/// Parse error
#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parse a test file
pub fn parse_test_file(path: &Path) -> Result<TestCase, ParseError> {
    let content = std::fs::read_to_string(path).map_err(|e| ParseError {
        line: 0,
        message: format!("Failed to read file: {}", e),
    })?;
    parse_test_content(&content)
}

/// Parse test content from a string
pub fn parse_test_content(content: &str) -> Result<TestCase, ParseError> {
    let mut name: Option<String> = None;
    let mut directories: Vec<Directory> = Vec::new();
    let mut configs: Vec<TestConfig> = Vec::new();
    let mut terminals: Vec<Terminal> = Vec::new();
    let mut steps: Vec<TestStep> = Vec::new();

    #[derive(Debug, PartialEq)]
    enum Section {
        Header,
        Environment,
        Test,
    }

    let mut current_section = Section::Header;
    let mut yaml_block: Vec<String> = Vec::new();
    let mut yaml_type: Option<&str> = None;
    let mut line_num = 0;

    // For grouping output lines
    let mut pending_output_lines: Vec<String> = Vec::new();

    let flush_yaml_block = |yaml_block: &mut Vec<String>,
                            yaml_type: &mut Option<&str>,
                            directories: &mut Vec<Directory>,
                            configs: &mut Vec<TestConfig>,
                            terminals: &mut Vec<Terminal>,
                            line_num: usize|
     -> Result<(), ParseError> {
        if yaml_block.is_empty() {
            return Ok(());
        }

        let yaml_content = yaml_block.join("\n");
        yaml_block.clear();

        match *yaml_type {
            Some("directory") => {
                let dir: Directory =
                    serde_yaml::from_str(&yaml_content).map_err(|e| ParseError {
                        line: line_num,
                        message: format!("Invalid directory YAML: {}", e),
                    })?;
                directories.push(dir);
            }
            Some("config") => {
                let config: TestConfig =
                    serde_yaml::from_str(&yaml_content).map_err(|e| ParseError {
                        line: line_num,
                        message: format!("Invalid config YAML: {}", e),
                    })?;
                configs.push(config);
            }
            Some("terminal") => {
                let terminal: Terminal =
                    serde_yaml::from_str(&yaml_content).map_err(|e| ParseError {
                        line: line_num,
                        message: format!("Invalid terminal YAML: {}", e),
                    })?;
                terminals.push(terminal);
            }
            _ => {}
        }
        *yaml_type = None;
        Ok(())
    };

    let flush_pending_output = |pending_output_lines: &mut Vec<String>,
                                steps: &mut Vec<TestStep>| {
        if !pending_output_lines.is_empty() {
            let output = pending_output_lines.join("\n");
            steps.push(TestStep::ExpectOutput(output));
            pending_output_lines.clear();
        }
    };

    for line in content.lines() {
        line_num += 1;
        let trimmed = line.trim();

        // Handle section headers
        if trimmed == "## Environment" {
            flush_yaml_block(
                &mut yaml_block,
                &mut yaml_type,
                &mut directories,
                &mut configs,
                &mut terminals,
                line_num,
            )?;
            current_section = Section::Environment;
            continue;
        }

        if trimmed == "## Test" {
            flush_yaml_block(
                &mut yaml_block,
                &mut yaml_type,
                &mut directories,
                &mut configs,
                &mut terminals,
                line_num,
            )?;
            current_section = Section::Test;
            continue;
        }

        match current_section {
            Section::Header => {
                // Parse metadata comments
                if let Some(rest) = trimmed.strip_prefix("# test:") {
                    name = Some(rest.trim().to_string());
                }
                // Ignore other comments in header
            }
            Section::Environment => {
                // Empty line flushes current YAML block
                if trimmed.is_empty() {
                    flush_yaml_block(
                        &mut yaml_block,
                        &mut yaml_type,
                        &mut directories,
                        &mut configs,
                        &mut terminals,
                        line_num,
                    )?;
                    continue;
                }

                // Skip comments
                if trimmed.starts_with('#') {
                    continue;
                }

                // Start of new YAML block
                if trimmed == "directory:" {
                    flush_yaml_block(
                        &mut yaml_block,
                        &mut yaml_type,
                        &mut directories,
                        &mut configs,
                        &mut terminals,
                        line_num,
                    )?;
                    yaml_type = Some("directory");
                    continue;
                }
                if trimmed == "config:" {
                    flush_yaml_block(
                        &mut yaml_block,
                        &mut yaml_type,
                        &mut directories,
                        &mut configs,
                        &mut terminals,
                        line_num,
                    )?;
                    yaml_type = Some("config");
                    continue;
                }
                if trimmed == "terminal:" {
                    flush_yaml_block(
                        &mut yaml_block,
                        &mut yaml_type,
                        &mut directories,
                        &mut configs,
                        &mut terminals,
                        line_num,
                    )?;
                    yaml_type = Some("terminal");
                    continue;
                }

                // Continuation of YAML block (indented lines)
                if yaml_type.is_some() && line.starts_with("  ") {
                    // Remove the leading 2 spaces for YAML parsing
                    yaml_block.push(line[2..].to_string());
                }
            }
            Section::Test => {
                // Skip empty lines and comments
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                // Terminal switch - flush any pending output first
                if let Some(rest) = trimmed.strip_prefix('@') {
                    flush_pending_output(&mut pending_output_lines, &mut steps);
                    steps.push(TestStep::SwitchTerminal(rest.to_string()));
                    continue;
                }

                // Input line - flush any pending output first
                if let Some(rest) = trimmed.strip_prefix("> ") {
                    flush_pending_output(&mut pending_output_lines, &mut steps);
                    steps.push(TestStep::Input(rest.to_string()));
                    continue;
                }
                // Handle > with no space (empty input)
                if trimmed == ">" {
                    flush_pending_output(&mut pending_output_lines, &mut steps);
                    steps.push(TestStep::Input(String::new()));
                    continue;
                }

                // Output line - accumulate (use original line to preserve leading whitespace)
                pending_output_lines.push(line.trim_end().to_string());
            }
        }
    }

    // Flush any remaining YAML block
    flush_yaml_block(
        &mut yaml_block,
        &mut yaml_type,
        &mut directories,
        &mut configs,
        &mut terminals,
        line_num,
    )?;

    // Flush any remaining output lines
    flush_pending_output(&mut pending_output_lines, &mut steps);

    // Validate required fields
    let name = name.ok_or_else(|| ParseError {
        line: 0,
        message: "Missing '# test: <name>' header".to_string(),
    })?;

    Ok(TestCase {
        name,
        directories,
        configs,
        terminals,
        steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_test() {
        let content = r#"# test: simple_echo
# description: Test basic echo functionality

## Environment

config:
  name: local

terminal:
  name: T1
  config: local

## Test

@T1
> hello world
hello world
"#;

        let test_case = parse_test_content(content).unwrap();

        assert_eq!(test_case.name, "simple_echo");
        assert_eq!(test_case.configs.len(), 1);
        assert_eq!(test_case.configs[0].name, "local");
        assert!(test_case.configs[0].host_id.is_none());
        assert_eq!(test_case.terminals.len(), 1);
        assert_eq!(test_case.terminals[0].name, "T1");
        assert_eq!(test_case.terminals[0].config, Some("local".to_string()));
        assert_eq!(test_case.steps.len(), 3);

        match &test_case.steps[0] {
            TestStep::SwitchTerminal(name) => assert_eq!(name, "T1"),
            _ => panic!("Expected SwitchTerminal"),
        }
        match &test_case.steps[1] {
            TestStep::Input(input) => assert_eq!(input, "hello world"),
            _ => panic!("Expected Input"),
        }
        match &test_case.steps[2] {
            TestStep::ExpectOutput(output) => assert_eq!(output, "hello world"),
            _ => panic!("Expected ExpectOutput"),
        }
    }

    #[test]
    fn test_parse_multi_terminal() {
        let content = r#"# test: multi_terminal

## Environment

config:
  name: local
  host_id: host-a

terminal:
  name: T1
  config: local

terminal:
  name: T2
  config: local

## Test

@T1
> first
first

@T2
> second
second

@T1
second
"#;

        let test_case = parse_test_content(content).unwrap();

        assert_eq!(test_case.configs.len(), 1);
        assert_eq!(test_case.configs[0].host_id, Some("host-a".to_string()));
        assert_eq!(test_case.terminals.len(), 2);
        assert_eq!(test_case.terminals[0].name, "T1");
        assert_eq!(test_case.terminals[1].name, "T2");

        // Count terminal switches
        let switches: Vec<_> = test_case
            .steps
            .iter()
            .filter_map(|s| {
                if let TestStep::SwitchTerminal(name) = s {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(switches, vec!["T1", "T2", "T1"]);
    }

    #[test]
    fn test_parse_grouped_output() {
        let content = r#"# test: grouped_output

## Environment

config:
  name: local

terminal:
  name: T1

## Test

@T1
> command
line1
line2
line3
"#;

        let test_case = parse_test_content(content).unwrap();

        // Should have: SwitchTerminal, Input, ExpectOutput (grouped)
        assert_eq!(test_case.steps.len(), 3);

        match &test_case.steps[2] {
            TestStep::ExpectOutput(output) => {
                assert_eq!(output, "line1\nline2\nline3");
            }
            _ => panic!("Expected ExpectOutput"),
        }
    }

    #[test]
    fn test_parse_directory() {
        let content = r#"# test: with_directory

## Environment

directory:
  name: cwd
  path: /tmp/test

config:
  name: local

terminal:
  name: T1
  cwd: cwd

## Test

@T1
> hello
hello
"#;

        let test_case = parse_test_content(content).unwrap();

        assert_eq!(test_case.directories.len(), 1);
        assert_eq!(test_case.directories[0].name, "cwd");
        assert_eq!(test_case.directories[0].path, Some("/tmp/test".to_string()));
        assert_eq!(test_case.terminals[0].cwd, Some("cwd".to_string()));
    }

    #[test]
    fn test_parse_minimal_terminal() {
        // Terminal with only name - config and cwd will be auto-injected
        let content = r#"# test: minimal

## Environment

terminal:
  name: T1

## Test

@T1
> hello
hello
"#;

        let test_case = parse_test_content(content).unwrap();
        assert_eq!(test_case.terminals.len(), 1);
        assert_eq!(test_case.terminals[0].name, "T1");
        assert!(test_case.terminals[0].config.is_none());
        assert!(test_case.terminals[0].cwd.is_none());
    }

    #[test]
    fn test_missing_test_name() {
        let content = r#"## Environment

config:
  name: local

terminal:
  name: T1
  config: local

## Test

@T1
> hello
"#;

        let result = parse_test_content(content);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("test:"));
    }
}
