//! Replays the Node `data` CLI golden (crates/proxi/tests/fixtures/golden/data-cli.json):
//! every case runs `proxi data <args>` with the case's env and compares the
//! normalised stdout/stderr/exit code with the frozen expectation.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::cli_golden;

/// `<home>`/`<repo>`/`<dataRoot>` → the replay's temp paths, for the case env values.
fn substitute(text: &str, home: &Path, repo: &Path, data: &Path) -> String {
    text.replace("<home>", &home.display().to_string())
        .replace("<repo>", &repo.display().to_string())
        .replace("<dataRoot>", &data.display().to_string())
}

/// The capture's normalisation: temp roots back to their tags, then separators.
fn normalise(text: &str, roots: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (root, tag) in roots {
        out = out.replace(root.as_str(), tag.as_str());
    }
    out.replace("\\\\", "/").replace('\\', "/")
}

fn roots(home: &Path, repo: &Path, data: &Path, repo_root: &Path) -> Vec<(String, String)> {
    vec![
        (data.display().to_string(), "<dataRoot>".to_string()),
        (repo.display().to_string(), "<repo>".to_string()),
        (home.display().to_string(), "<home>".to_string()),
        (repo_root.display().to_string(), "<repoRoot>".to_string()),
    ]
}

#[test]
fn data_cli_replays_the_node_golden() {
    let golden = cli_golden::golden("data-cli");
    let cases = golden["cases"].as_array().expect("cases array");
    let names: Vec<&str> = cases
        .iter()
        .map(|case| case["name"].as_str().expect("case name"))
        .collect();
    assert_eq!(
        names,
        vec![
            "where_env_absolute_matches_node",
            "where_env_relative_matches_node",
            "where_env_padded_matches_node",
            "where_config_pointer_matches_node",
            "where_config_pointer_relative_falls_back_to_default",
            "where_default_matches_node",
            "cli_where_matches_node",
            "cli_help_matches_node",
            "cli_no_args_matches_node",
            "cli_unknown_command_matches_node",
            "cli_unknown_long_option_matches_node",
            "cli_unknown_long_option_with_value_matches_node",
            "cli_unknown_short_option_matches_node",
            "cli_unknown_short_option_group_matches_node",
            "cli_double_dash_positional_matches_node",
            "cli_double_dash_unknown_command_matches_node",
        ],
        "golden case names"
    );

    // The cwd is pinned to the repo root: the relative-env case resolves against it.
    let repo_root = common::repo_root();

    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let home = common::temp_dir("data-cli-home");
        let repo = common::temp_dir("data-cli-repo");
        let data = common::temp_dir("data-cli-data");
        let roots = roots(&home.0, &repo.0, &data.0, &repo_root);

        // The config pointer the case resolves through, or none.
        match &case["config"] {
            serde_json::Value::Null => {}
            config => {
                let data_root = config["dataRoot"].as_str().expect("config dataRoot");
                let body = serde_json::json!({
                    "dataRoot": substitute(data_root, &home.0, &repo.0, &data.0)
                });
                fs::create_dir_all(repo.0.join(".learner")).expect("create .learner");
                fs::write(
                    repo.0.join(".learner").join("config.json"),
                    format!("{body}\n"),
                )
                .expect("write config");
            }
        }

        let mut command = Command::new(common::proxi_bin());
        command.arg("data").current_dir(&repo_root);
        for arg in case["args"].as_array().expect("case args") {
            command.arg(arg.as_str().expect("arg"));
        }
        for (key, value) in case["env"].as_object().expect("case env") {
            command.env(
                key,
                substitute(value.as_str().expect("env value"), &home.0, &repo.0, &data.0),
            );
        }
        // Vars the golden does not set must not leak in from the parent environment.
        for key in ["LEARNER_DATA_ROOT", "LEARNER_REPO_ROOT"] {
            if case["env"].get(key).is_none() {
                command.env_remove(key);
            }
        }

        let output = command
            .output()
            .unwrap_or_else(|error| panic!("{name}: run proxi data: {error}"));
        assert_eq!(
            normalise(&String::from_utf8_lossy(&output.stdout), &roots),
            case["stdout"].as_str().expect("golden stdout"),
            "{name}: stdout"
        );
        assert_eq!(
            normalise(&String::from_utf8_lossy(&output.stderr), &roots),
            case["stderr"].as_str().expect("golden stderr"),
            "{name}: stderr"
        );
        assert_eq!(
            output.status.code(),
            Some(case["exitCode"].as_i64().expect("golden exit code") as i32),
            "{name}: exit code"
        );
    }
}
