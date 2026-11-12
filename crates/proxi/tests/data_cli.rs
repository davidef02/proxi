mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn assert_parity(
    label: &str,
    bin_args: &[&str],
    node_args: &[&str],
    env_data_root: Option<&Path>,
    repo: &Path,
    home: &Path,
) {
    let mut node = common::node(node_args);
    let mut bin = Command::new(common::proxi_bin());
    bin.arg("data").args(bin_args);

    for command in [&mut node, &mut bin] {
        command
            .current_dir(common::repo_root())
            .env("HOME", home)
            .env("USERPROFILE", home);
        match env_data_root {
            Some(value) => command.env("LEARNER_DATA_ROOT", value),
            None => command.env_remove("LEARNER_DATA_ROOT"),
        };
    }
    // Node's `data` CLI ignores this; the binary uses it to read the temp repo's config.
    bin.env("LEARNER_REPO_ROOT", repo);

    let expected = node.output().expect("run node");
    let actual = bin.output().expect("run proxi");

    assert_eq!(
        text(&actual.stdout),
        text(&expected.stdout),
        "{label}: stdout"
    );
    assert_eq!(
        text(&actual.stderr),
        text(&expected.stderr),
        "{label}: stderr"
    );
    assert_eq!(
        actual.status.code(),
        expected.status.code(),
        "{label}: exit code"
    );
}

/// `proxi data where` vs the real `resolveDataRoot` called by the Node helper.
fn resolve_case(label: &str, env_data_root: Option<&Path>, repo: &Path, home: &Path) {
    let node_args = [
        "crates/proxi/tests/node/resolve-data-root.ts",
        repo.to_str().expect("utf-8 repo"),
        home.to_str().expect("utf-8 home"),
    ];
    assert_parity(label, &["where"], &node_args, env_data_root, repo, home);
}

/// `proxi data <args>` vs the real Node CLI.
fn cli_case(label: &str, args: &[&str]) {
    let home = common::temp_dir("home");
    let repo = common::temp_dir("repo");
    let data = common::temp_dir("env-data");
    let mut node_args = vec!["extensions/data-root.ts"];
    node_args.extend_from_slice(args);
    assert_parity(label, args, &node_args, Some(&data.0), &repo.0, &home.0);
}

#[test]
fn where_env_absolute_matches_node() {
    common::require_node();
    let home = common::temp_dir("home");
    let repo = common::temp_dir("repo");
    let data = common::temp_dir("data");
    resolve_case("env absolute", Some(&data.0), &repo.0, &home.0);
}

#[test]
fn where_env_relative_matches_node() {
    common::require_node();
    let home = common::temp_dir("home");
    let repo = common::temp_dir("repo");
    resolve_case("env relative", Some(Path::new("rel/dir")), &repo.0, &home.0);
}

#[test]
fn where_env_padded_matches_node() {
    common::require_node();
    let home = common::temp_dir("home");
    let repo = common::temp_dir("repo");
    let data = common::temp_dir("data");
    let padded = format!("  {}  ", data.0.display());
    resolve_case("env padded", Some(Path::new(&padded)), &repo.0, &home.0);
}

#[test]
fn where_config_pointer_matches_node() {
    common::require_node();
    let home = common::temp_dir("home");
    let repo = common::temp_dir("repo");
    let data = common::temp_dir("data");
    fs::create_dir_all(repo.0.join(".learner")).expect("create .learner");
    let body = serde_json::json!({ "dataRoot": data.0.to_str().expect("utf-8 data") }).to_string();
    fs::write(repo.0.join(".learner").join("config.json"), body).expect("write config");
    resolve_case("config pointer", None, &repo.0, &home.0);
}

#[test]
fn where_config_pointer_relative_falls_back_to_default() {
    common::require_node();
    let home = common::temp_dir("home");
    let repo = common::temp_dir("repo");
    fs::create_dir_all(repo.0.join(".learner")).expect("create .learner");
    fs::write(
        repo.0.join(".learner").join("config.json"),
        r#"{"dataRoot":"rel/dir"}"#,
    )
    .expect("write config");
    resolve_case("config relative", None, &repo.0, &home.0);
}

#[test]
fn where_default_matches_node() {
    common::require_node();
    let home = common::temp_dir("home");
    let repo = common::temp_dir("repo");
    resolve_case("default", None, &repo.0, &home.0);
}

#[test]
fn cli_where_matches_node() {
    common::require_node();
    cli_case("where", &["where"]);
}

#[test]
fn cli_help_matches_node() {
    common::require_node();
    cli_case("help", &["help"]);
}

#[test]
fn cli_no_args_matches_node() {
    common::require_node();
    cli_case("no args", &[]);
}

#[test]
fn cli_unknown_command_matches_node() {
    common::require_node();
    cli_case("unknown command", &["nope"]);
}

#[test]
fn cli_unknown_long_option_matches_node() {
    common::require_node();
    cli_case("unknown long option", &["where", "--foo"]);
}

#[test]
fn cli_unknown_long_option_with_value_matches_node() {
    common::require_node();
    cli_case("unknown long option with value", &["where", "--foo=bar"]);
}

#[test]
fn cli_unknown_short_option_matches_node() {
    common::require_node();
    cli_case("unknown short option", &["-x"]);
}

#[test]
fn cli_unknown_short_option_group_matches_node() {
    common::require_node();
    cli_case("unknown short option group", &["-abc"]);
}

#[test]
fn cli_double_dash_positional_matches_node() {
    common::require_node();
    cli_case("double dash positional", &["where", "--", "--zz"]);
}

#[test]
fn cli_double_dash_unknown_command_matches_node() {
    common::require_node();
    cli_case("double dash unknown command", &["--", "-abc"]);
}
