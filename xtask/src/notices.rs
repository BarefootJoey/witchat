//! Dependency notices from `cargo metadata`: every crate linked into the
//! app for this target (normal dependencies only; build scripts and
//! dev-dependencies are not shipped), with license and source.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

pub fn package_version(metadata: &Value, name: &str) -> Result<String> {
    packages(metadata)?
        .iter()
        .find(|p| p["name"] == name)
        .and_then(|p| p["version"].as_str())
        .map(str::to_string)
        .with_context(|| format!("{name} not in cargo metadata"))
}

pub fn render(metadata: &Value, root: &str) -> Result<String> {
    let shipped = shipped_ids(metadata, root)?;
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .context("no workspace_members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();

    let mut rows: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for package in packages(metadata)? {
        let id = package["id"].as_str().unwrap_or_default();
        if !shipped.contains(id) || members.contains(id) {
            continue;
        }
        let field = |key: &str| package[key].as_str().unwrap_or("").to_string();
        let license = match field("license") {
            l if !l.is_empty() => l,
            _ if !field("license_file").is_empty() => "see license file in crate".into(),
            _ => "UNKNOWN".into(),
        };
        rows.insert((field("name"), field("version")), (license, field("repository")));
    }

    let mut out = String::from(
        "witchat third-party notices\n\
         Generated from Cargo.lock by `cargo run -p witchat-xtask -- dist`.\n\
         Crates linked into this binary, with their declared SPDX license.\n\
         Where an expression offers more than one license, this build uses a permissive option.\n\
         Full license texts ship inside each crate's source on crates.io.\n\n",
    );
    for ((name, version), (license, repo)) in &rows {
        out.push_str(&format!("{name} {version}\n  license: {license}\n"));
        if !repo.is_empty() {
            out.push_str(&format!("  source:  {repo}\n"));
        }
    }
    out.push_str(&format!("\n{} crates\n", rows.len()));
    Ok(out)
}

/// Licenses a shipped crate may use: MIT/BSD/Apache plus other permissive,
/// no-copyleft terms. `Unicode-3.0` covers ICU data tables;
/// `CDLA-Permissive-2.0` covers the Mozilla root list in `webpki-roots`.
const ALLOWED: &[&str] = &[
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "0BSD",
    "Zlib",
    "Unlicense",
    "CC0-1.0",
    "BSL-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "CDLA-Permissive-2.0",
];

/// Shipped third-party crates whose license has no allowed option, as
/// `name version (license)`. Empty means the binary may be published.
pub fn disallowed(metadata: &Value, root: &str) -> Result<Vec<String>> {
    let shipped = shipped_ids(metadata, root)?;
    let mut bad: Vec<String> = packages(metadata)?
        .iter()
        .filter(|p| shipped.contains(p["id"].as_str().unwrap_or_default()))
        .filter(|p| p["name"] != root && p["source"].is_string())
        .filter_map(|p| {
            let license = p["license"].as_str().unwrap_or("");
            let row = format!("{} {} ({})", p["name"].as_str()?, p["version"].as_str()?, license);
            (!license_allowed(license)).then_some(row)
        })
        .collect();
    bad.sort();
    bad.dedup();
    Ok(bad)
}

/// Evaluates an SPDX expression (`AND` binds tighter than `OR`,
/// parentheses, legacy `/` as `OR`, `WITH` exceptions ignored). An empty
/// expression is not allowed: a bare license file needs a human.
pub fn license_allowed(expr: &str) -> bool {
    let spaced = expr.replace('(', " ( ").replace(')', " ) ").replace('/', " OR ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut pos = 0;
    !tokens.is_empty() && spdx_or(&tokens, &mut pos) == Some(true) && pos == tokens.len()
}

fn spdx_or(t: &[&str], pos: &mut usize) -> Option<bool> {
    let mut ok = spdx_and(t, pos)?;
    while t.get(*pos) == Some(&"OR") {
        *pos += 1;
        ok |= spdx_and(t, pos)?;
    }
    Some(ok)
}

fn spdx_and(t: &[&str], pos: &mut usize) -> Option<bool> {
    let mut ok = spdx_term(t, pos)?;
    while t.get(*pos) == Some(&"AND") {
        *pos += 1;
        ok &= spdx_term(t, pos)?;
    }
    Some(ok)
}

fn spdx_term(t: &[&str], pos: &mut usize) -> Option<bool> {
    let token = *t.get(*pos)?;
    *pos += 1;
    let ok = if token == "(" {
        let inner = spdx_or(t, pos)?;
        (t.get(*pos) == Some(&")")).then(|| *pos += 1)?;
        inner
    } else {
        ALLOWED.contains(&token)
    };
    if t.get(*pos) == Some(&"WITH") {
        *pos += 2;
    }
    Some(ok)
}

fn packages(metadata: &Value) -> Result<&Vec<Value>> {
    metadata["packages"].as_array().context("cargo metadata has no packages")
}

fn is_proc_macro(package: &Value) -> bool {
    package["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["kind"].as_array().into_iter().flatten())
        .any(|k| k == "proc-macro")
}

/// Walks the resolve graph from `root` along normal (non-build, non-dev)
/// edges. Proc-macros run in the compiler, so neither they nor their own
/// dependencies end up in the binary.
fn shipped_ids(metadata: &Value, root: &str) -> Result<BTreeSet<String>> {
    let proc_macros: BTreeSet<&str> =
        packages(metadata)?.iter().filter(|p| is_proc_macro(p)).filter_map(|p| p["id"].as_str()).collect();
    let root_id = packages(metadata)?
        .iter()
        .find(|p| p["name"] == root)
        .and_then(|p| p["id"].as_str())
        .with_context(|| format!("{root} not in cargo metadata"))?
        .to_string();
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .context("cargo metadata has no resolve graph")?
        .iter()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();

    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([root_id]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(node) = nodes.get(id.as_str()) else { continue };
        for dep in node["deps"].as_array().into_iter().flatten() {
            let normal = dep["dep_kinds"].as_array().into_iter().flatten().any(|k| k["kind"].is_null());
            match dep["pkg"].as_str() {
                Some(pkg) if normal && !proc_macros.contains(pkg) => queue.push_back(pkg.to_string()),
                _ => {}
            }
        }
    }
    Ok(seen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "workspace_members": ["app"],
            "packages": [
                {"id": "app", "name": "witchat-app", "version": "0.1.0", "license": "MIT", "repository": null},
                {"id": "a", "name": "alpha", "version": "1.0.0", "license": "MIT OR Apache-2.0", "repository": "https://example.com/alpha"},
                {"id": "b", "name": "beta", "version": "2.0.0", "license": null, "license_file": "LICENSE", "repository": null},
                {"id": "build", "name": "cc", "version": "1.2.0", "license": "MIT", "repository": null},
                {"id": "dev", "name": "proptest", "version": "1.0.0", "license": "MIT", "repository": null},
                {"id": "pm", "name": "serde_derive", "version": "1.0.0", "license": "MIT", "repository": null,
                 "targets": [{"kind": ["proc-macro"]}]},
                {"id": "syn", "name": "syn", "version": "2.0.0", "license": "MIT", "repository": null}
            ],
            "resolve": {"nodes": [
                {"id": "app", "deps": [
                    {"pkg": "a", "dep_kinds": [{"kind": null}]},
                    {"pkg": "build", "dep_kinds": [{"kind": "build"}]},
                    {"pkg": "dev", "dep_kinds": [{"kind": "dev"}]},
                    {"pkg": "pm", "dep_kinds": [{"kind": null}]}
                ]},
                {"id": "a", "deps": [{"pkg": "b", "dep_kinds": [{"kind": null}]}]},
                {"id": "b", "deps": []},
                {"id": "build", "deps": []},
                {"id": "dev", "deps": []},
                {"id": "pm", "deps": [{"pkg": "syn", "dep_kinds": [{"kind": null}]}]},
                {"id": "syn", "deps": []}
            ]}
        })
    }

    #[test]
    fn lists_only_shipped_third_party_crates() {
        let text = render(&sample(), "witchat-app").unwrap();
        assert!(text.contains("alpha 1.0.0\n  license: MIT OR Apache-2.0\n  source:  https://example.com/alpha"));
        assert!(text.contains("beta 2.0.0\n  license: see license file in crate"));
        assert!(!text.contains("witchat-app"), "workspace crate listed");
        assert!(!text.contains("cc 1.2.0"), "build dependency listed");
        assert!(!text.contains("proptest"), "dev dependency listed");
        assert!(!text.contains("serde_derive"), "proc-macro listed");
        assert!(!text.contains("syn 2.0.0"), "proc-macro dependency listed");
        assert!(text.ends_with("2 crates\n"));
    }

    #[test]
    fn spdx_expressions_need_one_permissive_option() {
        for ok in [
            "MIT",
            "MIT OR Apache-2.0",
            "MIT/Apache-2.0",
            "Apache-2.0 AND ISC",
            "Apache-2.0 WITH LLVM-exception OR MIT",
            "(MIT OR Apache-2.0) AND Unicode-3.0",
            "Apache-2.0 OR GPL-2.0-only",
        ] {
            assert!(license_allowed(ok), "{ok}");
        }
        for bad in ["", "MPL-2.0", "GPL-3.0-only", "MIT AND LGPL-2.1", "(MIT", "MIT OR"] {
            assert!(!license_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn copyleft_and_bare_license_files_block_dist() {
        let mut meta = sample();
        for p in meta["packages"].as_array_mut().unwrap() {
            p["source"] = json!("registry+https://github.com/rust-lang/crates.io-index");
        }
        meta["packages"][1]["license"] = json!("MPL-2.0");
        let bad = disallowed(&meta, "witchat-app").unwrap();
        assert_eq!(bad, ["alpha 1.0.0 (MPL-2.0)", "beta 2.0.0 ()"]);
    }

    #[test]
    fn reads_app_version() {
        assert_eq!(package_version(&sample(), "witchat-app").unwrap(), "0.1.0");
    }
}
