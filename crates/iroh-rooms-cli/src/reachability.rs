//! Pins the CLI command tree to `docs/reachability-inventory.md`.
//!
//! Refs #343. A subcommand or flag that clap accepts but the inventory does not
//! list, or the reverse, fails this module. The removed-command fence is the
//! list of advertised names that must stay absent.

use std::collections::BTreeSet;
use std::path::PathBuf;

use clap::{Arg, Command, CommandFactory};

use crate::cli::Cli;

const REMOVED: &[&str] = &[
    "iroh-rooms agent drive",
    "iroh-rooms file serve",
    "iroh-rooms file unpin",
    "iroh-rooms identity export",
    "iroh-rooms room encrypt",
    "iroh-rooms room export",
    "iroh-rooms room list",
    "iroh-rooms room rotate",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn inventory() -> String {
    let path = workspace_root().join("docs/reachability-inventory.md");
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn read_fence(doc: &str, name: &str) -> BTreeSet<String> {
    let needle = format!("```text\n{name}\n");
    let start = doc
        .find(&needle)
        .unwrap_or_else(|| panic!("docs/reachability-inventory.md is missing the {name} fence"));
    let body = &doc[start + needle.len()..];
    let end = body
        .find("```")
        .unwrap_or_else(|| panic!("docs/reachability-inventory.md {name} fence is unclosed"));
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect()
}

fn command_tree() -> BTreeSet<String> {
    let mut rows = BTreeSet::new();
    walk(&Cli::command(), "iroh-rooms", &mut rows);
    rows
}

fn walk(cmd: &Command, path: &str, rows: &mut BTreeSet<String>) {
    let mut row = format!("command {path}");
    if cmd.is_hide_set() {
        row.push_str(" hidden");
    }
    rows.insert(row);
    let at_root = path == "iroh-rooms";
    for arg in cmd.get_arguments() {
        push_arg(arg, path, at_root, rows);
    }
    for sub in cmd.get_subcommands() {
        let child = format!("{path} {}", sub.get_name());
        walk(sub, &child, rows);
    }
}

fn push_arg(arg: &Arg, path: &str, at_root: bool, rows: &mut BTreeSet<String>) {
    let id = arg.get_id().as_str();
    if id == "help" || id == "version" {
        return;
    }
    // Globals are part of the root command. Clap also surfaces them on
    // subcommands; listing them once keeps the inventory equal to the product
    // surface rather than clap's propagation copy.
    if arg.is_global_set() && !at_root {
        return;
    }
    if let Some(long) = arg.get_long() {
        let mut row = format!("flag {path} --{long}");
        if arg.is_hide_set() {
            row.push_str(" hidden");
        }
        rows.insert(row);
        return;
    }
    if arg.get_short().is_some() {
        return;
    }
    if let Some(names) = arg.get_value_names() {
        for name in names {
            rows.insert(format!("arg {path} <{name}>"));
        }
    }
}

#[test]
fn inventory_cli_fence_matches_clap_command_tree() {
    let expected = read_fence(&inventory(), "reachability-cli");
    let actual = command_tree();
    let missing: Vec<_> = actual.difference(&expected).cloned().collect();
    let extra: Vec<_> = expected.difference(&actual).cloned().collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "CLI reachability inventory drifted from clap\n\n\
         in clap, missing from the inventory:\n{}\n\n\
         in the inventory, absent from clap:\n{}\n",
        missing.join("\n"),
        extra.join("\n"),
    );
}

#[test]
fn removed_commands_stay_out_of_the_command_tree() {
    let expected: BTreeSet<String> = REMOVED.iter().map(|s| (*s).to_string()).collect();
    let documented = read_fence(&inventory(), "reachability-cli-removed");
    assert_eq!(
        documented, expected,
        "the removed-command fence must stay equal to the pinned list"
    );
    let tree = command_tree();
    for name in REMOVED {
        let row = format!("command {name}");
        assert!(
            !tree.contains(&row),
            "{name} is back in clap; it was an advertised command with no handler"
        );
    }
}

#[test]
fn agent_help_does_not_advertise_a_drive_command() {
    let agent = Cli::command()
        .find_subcommand("agent")
        .expect("agent subcommand")
        .clone();
    let about = agent
        .get_about()
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(
        !about.to_lowercase().contains("drive"),
        "agent help still advertises a drive capability that has no subcommand: {about}"
    );
    assert!(
        agent.find_subcommand("drive").is_none(),
        "agent drive must not be a subcommand"
    );
}
