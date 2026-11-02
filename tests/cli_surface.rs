//! Black-box tests over the built binary. Fully offline: nothing here touches
//! the network or the OS keychain — SPEC v1 §1.2 requires arguments to fail
//! before either is reached, and these tests hold the binary to it.

use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn alp() -> Command {
    let mut c = Command::cargo_bin("alp").expect("binary builds");
    // The developer's shell must not steer a test.
    c.env_remove("ALP_ENV");
    c
}

/// A throwaway config path, so a test never reads or writes the developer's
/// real `~/.config/alp` settings.
fn scratch_config(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("alp-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("config.json")
}

fn isolated(tag: &str) -> Command {
    let mut c = alp();
    c.arg("--config").arg(scratch_config(tag));
    c
}

const GROUPS: &[&str] = &[
    "auth",
    "config",
    "account",
    "positions",
    "transactions",
    "dividends",
    "api",
    "mcp",
    "self-update",
    "completions",
    "info",
];

#[test]
fn top_level_help_lists_the_surface() {
    let out = alp().arg("--help").assert().success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    for g in GROUPS {
        assert!(text.contains(g), "top-level help is missing `{g}`:\n{text}");
    }
    // The global flags the family spec requires, plus this CLI's --env.
    for flag in [
        "--json",
        "--verbose",
        "--quiet",
        "--no-color",
        "--env",
        "--config",
    ] {
        assert!(text.contains(flag), "top-level help is missing `{flag}`");
    }
}

/// Rendering a subcommand's help forces clap's debug assertions to run over
/// that subtree, which is what catches conflicting short flags.
#[test]
fn every_subcommand_help_renders() {
    for g in GROUPS {
        alp().arg(g).arg("--help").assert().success();
    }
}

#[test]
fn nested_subcommand_help_renders() {
    let nested = [
        ["auth", "login"],
        ["auth", "status"],
        ["auth", "logout"],
        ["auth", "set-credential"],
        ["config", "set"],
        ["config", "unset"],
        ["positions", "list"],
        ["transactions", "list"],
        ["dividends", "list"],
    ];
    for pair in nested {
        alp()
            .args(pair)
            .arg("--help")
            .assert()
            .success()
            .stdout(contains("Usage"));
    }
}

#[test]
fn version_prints_the_crate_version() {
    alp()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn info_emits_cli_info_dto() {
    let out = alp().args(["info", "--json"]).assert().success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("info emits JSON");
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["spec"], "piekstra-cli/1");
    assert_eq!(v["name"], "alp");
    assert_eq!(v["auth"]["required"], true);
    assert_eq!(v["auth"]["method"], "api-key");
    let caps = v["capabilities"].as_array().expect("capabilities array");
    for expected in ["account", "positions", "transactions", "dividends", "api"] {
        assert!(
            caps.iter().any(|c| c == expected),
            "capabilities missing {expected}"
        );
    }
}

#[test]
fn unknown_command_is_a_usage_error() {
    alp().arg("nope").assert().code(2);
}

/// Dates are validated before config, keychain, or network, so this is a
/// usage error even on a machine that has never stored a credential.
#[test]
fn bad_date_is_a_usage_error() {
    isolated("baddate")
        .args(["transactions", "list", "--since", "06/30/2026"])
        .assert()
        .code(2);
    isolated("baddatediv")
        .args(["dividends", "list", "--until", "not-a-date"])
        .assert()
        .code(2);
}

#[test]
fn inverted_range_is_a_usage_error() {
    isolated("inverted")
        .args([
            "transactions",
            "list",
            "--since",
            "2026-12-31",
            "--until",
            "2026-01-01",
        ])
        .assert()
        .code(2);
}

#[test]
fn an_empty_type_filter_is_a_usage_error() {
    isolated("emptytype")
        .args(["transactions", "list", "--type", ""])
        .assert()
        .code(2);
}

#[test]
fn json_errors_use_the_family_error_dto() {
    let out = isolated("jsonerr")
        .args(["transactions", "list", "--since", "nope", "--json"])
        .assert()
        .code(2);
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("error DTO on stdout");
    assert_eq!(v["error"]["code"], "usage");
    assert!(v["error"]["message"].is_string());
}

/// Read-only is a safety property, so a write verb must be refused before the
/// keychain or the network is touched.
#[test]
fn api_refuses_anything_but_get() {
    for method in ["POST", "PUT", "DELETE", "patch"] {
        isolated("apiverb")
            .args(["api", method, "/v2/orders"])
            .assert()
            .code(2)
            .stderr(contains("read-only"));
    }
}

#[test]
fn api_refuses_a_body() {
    isolated("apibody")
        .args(["api", "GET", "/v2/account", "--data", "{}"])
        .assert()
        .code(2)
        .stderr(contains("read-only"));
}

/// The passthrough sends the account's key headers, so an absolute URL off
/// the configured origin — including the lookalike-host shape — must be a
/// usage error *before* the credential is read.
#[test]
fn api_refuses_off_origin_urls_before_the_keychain() {
    for url in [
        "https://api.alpaca.markets.evil.example/v2/account",
        "https://data.alpaca.markets/v2/stocks/bars",
    ] {
        isolated("apiorigin")
            .args(["api", "GET", url])
            .assert()
            .code(2)
            .stderr(contains("refusing"));
    }
}

/// `--env` steers which origin the passthrough accepts.
#[test]
fn the_env_flag_switches_the_api_origin() {
    // The live host is off-origin when paper is selected…
    isolated("envorigin")
        .args([
            "--env",
            "paper",
            "api",
            "GET",
            "https://api.alpaca.markets/v2/account",
        ])
        .assert()
        .code(2);
    // …and an unknown environment is a usage error on its own.
    isolated("envbad")
        .args(["--env", "prod", "account"])
        .assert()
        .code(2)
        .stderr(contains("live"));
}

/// `auth login` argument failures must not open the keychain or prompt.
#[test]
fn login_source_conflicts_fail_as_usage() {
    isolated("loginboth")
        .args(["auth", "login", "--stdin", "--from-env", "ALP_TEST_UNSET"])
        .assert()
        .code(2);
    isolated("loginni")
        .args(["auth", "login", "--non-interactive"])
        .assert()
        .code(2)
        .stderr(contains("--stdin"));
    isolated("loginenv")
        .args(["auth", "login", "--from-env", "ALP_TEST_UNSET_VAR"])
        .assert()
        .code(2);
}

/// Malformed credential input fails during parsing — before the keychain.
#[test]
fn a_half_credential_on_stdin_is_a_usage_error() {
    isolated("loginhalf")
        .args(["auth", "login", "--stdin"])
        .write_stdin("just-a-key-id-no-secret")
        .assert()
        .code(2)
        .stderr(contains("secret"));
}

/// `set-credential` requires an explicit source; with none it must fail as
/// usage without touching the keychain.
#[test]
fn set_credential_requires_an_explicit_source() {
    isolated("setcred")
        .args(["auth", "set-credential"])
        .assert()
        .code(2)
        .stderr(contains("--stdin"));
}

#[test]
fn config_roundtrips_the_env_key() {
    let cfg = scratch_config("configset");
    let _ = std::fs::remove_file(&cfg);

    alp()
        .arg("--config")
        .arg(&cfg)
        .args(["config", "set", "env", "paper"])
        .assert()
        .success();

    let out = alp()
        .arg("--config")
        .arg(&cfg)
        .args(["config", "show", "--json"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("config show emits JSON");
    assert_eq!(v["env"], "paper");

    alp()
        .arg("--config")
        .arg(&cfg)
        .args(["config", "unset", "env"])
        .assert()
        .success();
}

#[test]
fn config_validates_its_values() {
    isolated("cfgbadenv")
        .args(["config", "set", "env", "prod"])
        .assert()
        .code(2);
    isolated("cfgbadurl")
        .args(["config", "set", "base_url", "http://api.alpaca.markets"])
        .assert()
        .code(2)
        .stderr(contains("https"));
}

/// Secrets are keychain-only; the config file must never accept one, and the
/// refusal should say where keys actually go.
#[test]
fn config_rejects_secret_shaped_keys() {
    for key in ["secret_key", "api_key", "key_id"] {
        isolated("cfgsecret")
            .args(["config", "set", key, "value"])
            .assert()
            .code(2)
            .stderr(contains("keychain"));
    }
}

#[test]
fn config_path_prints_the_override() {
    let cfg = scratch_config("cfgpath");
    alp()
        .arg("--config")
        .arg(&cfg)
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(contains("alp-test-"));
}

#[test]
fn completions_generate_for_each_shell() {
    for shell in ["bash", "zsh", "fish"] {
        alp()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicates::str::is_empty().not());
    }
}
