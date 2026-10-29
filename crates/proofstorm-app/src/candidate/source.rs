use crate::Error;
use proofstorm_core::{CandidateInput, is_github_repository};
use serde_json::Value;

pub(super) async fn resolve(
    source: &CandidateInput,
    repository: &str,
) -> Result<(String, String), Error> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("proofstorm/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::failure(e.to_string(), None))?;
    resolve_with(source, repository, |repository, path| {
        let client = client.clone();
        async move { get(&client, &repository, &path).await }
    })
    .await
}

/// `get` reads `/repos/{repository}/{path..}` from the GitHub API.
async fn resolve_with<F, Fut>(
    source: &CandidateInput,
    repository: &str,
    get: F,
) -> Result<(String, String), Error>
where
    F: Fn(String, Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Value, Error>>,
{
    match source {
        CandidateInput::Commit {
            sha: Some(sha),
            url: None,
            repository: None,
        } => Ok((format!("https://github.com/{repository}.git"), sha.clone())),
        CandidateInput::Commit {
            sha: Some(commit),
            url: None,
            repository: Some(fork),
        } => resolve_fork(repository, fork, commit, get).await,
        CandidateInput::PullRequest { url } => {
            let number = url
                .rsplit('/')
                .next()
                .ok_or_else(|| Error::problem("candidate_source_invalid", "Missing PR number"))?;
            let response = get(repository.into(), vec!["pulls".into(), number.into()]).await?;
            if response["base"]["repo"]["full_name"].as_str() != Some(repository)
                || response["base"]["repo"]["private"] != false
                || response["head"]["repo"]["private"] != false
            {
                return Err(Error::problem(
                    "candidate_repository_mismatch",
                    "PR must target the canonical public repository and have a public head repository",
                ));
            }
            let head = response["head"]["repo"]["full_name"]
                .as_str()
                .filter(|name| is_github_repository(name))
                .ok_or_else(|| {
                    Error::problem("candidate_source_invalid", "Invalid public head repository")
                })?;
            Ok((
                format!("https://github.com/{head}.git"),
                sha(&response["head"]["sha"])?,
            ))
        }
        CandidateInput::Tag { tag } => {
            let response = get(
                repository.into(),
                vec!["git".into(), "ref".into(), "tags".into(), tag.clone()],
            )
            .await?;
            let mut object = response["object"].clone();
            for _ in 0..8 {
                let id = sha(&object["sha"])?;
                match object["type"].as_str() {
                    Some("commit") => {
                        return Ok((format!("https://github.com/{repository}.git"), id));
                    }
                    Some("tag") => {
                        object = get(repository.into(), vec!["git".into(), "tags".into(), id])
                            .await?["object"]
                            .clone();
                    }
                    _ => {
                        return Err(Error::problem(
                            "candidate_tag_invalid",
                            "Tag must resolve to a commit",
                        ));
                    }
                }
            }
            Err(Error::problem(
                "candidate_tag_invalid",
                "Annotated tag nesting exceeds eight objects",
            ))
        }
        CandidateInput::Bundle {
            path: None,
            commit,
            sha256: Some(_),
            prerequisites,
        } => resolve_bundle(repository, commit, prerequisites, get).await,
        CandidateInput::Commit { .. } | CandidateInput::Bundle { .. } => Err(Error::problem(
            "candidate_source_invalid",
            "Source was not normalized",
        )),
    }
}

/// Check only a bundle's public prerequisites; its private tip never reaches GitHub.
async fn resolve_bundle<F, Fut>(
    repository: &str,
    commit: &str,
    prerequisites: &[String],
    get: F,
) -> Result<(String, String), Error>
where
    F: Fn(String, Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Value, Error>>,
{
    for prerequisite in prerequisites {
        let object = get(
            repository.into(),
            vec!["git".into(), "commits".into(), prerequisite.clone()],
        )
        .await
        .map_err(|error| {
            if !error.message.contains("404") {
                return error;
            }
            Error::problem(
                "candidate_bundle_base_missing",
                format!(
                    "Bundle prerequisite {prerequisite} is not a public commit of {repository}; base the bundle on pushed public history"
                ),
            )
        })?;
        if sha(&object["sha"])? != *prerequisite {
            return Err(Error::problem(
                "candidate_source_invalid",
                "GitHub returned a different commit",
            ));
        }
    }
    Ok((
        format!("https://github.com/{repository}.git"),
        commit.into(),
    ))
}

/// Accept a commit only from a public fork whose parent or network root is the
/// canonical repository, and only after GitHub confirms the commit object.
async fn resolve_fork<F, Fut>(
    repository: &str,
    fork: &str,
    commit: &str,
    get: F,
) -> Result<(String, String), Error>
where
    F: Fn(String, Vec<String>) -> Fut,
    Fut: std::future::Future<Output = Result<Value, Error>>,
{
    let response = get(fork.into(), Vec::new()).await?;
    let upstream = |key: &str| {
        response[key]["full_name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(repository))
    };
    let name = response["full_name"]
        .as_str()
        .filter(|name| is_github_repository(name) && name.eq_ignore_ascii_case(fork));
    let Some(name) = name.filter(|_| {
        response["private"] == false
            && response["fork"] == true
            && (upstream("parent") || upstream("source"))
    }) else {
        return Err(Error::problem(
            "candidate_repository_mismatch",
            "Commit repository must be a public fork of the component's canonical repository",
        ));
    };
    let object = get(
        fork.into(),
        vec!["git".into(), "commits".into(), commit.into()],
    )
    .await?;
    if sha(&object["sha"])? != commit {
        return Err(Error::problem(
            "candidate_source_invalid",
            "GitHub returned a different commit",
        ));
    }
    Ok((format!("https://github.com/{name}.git"), commit.into()))
}

fn sha(value: &Value) -> Result<String, Error> {
    value
        .as_str()
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| {
            Error::problem(
                "candidate_source_invalid",
                "GitHub did not return a full commit SHA",
            )
        })
}

async fn get(client: &reqwest::Client, repository: &str, path: &[String]) -> Result<Value, Error> {
    let mut url = reqwest::Url::parse(&format!("https://api.github.com/repos/{repository}/"))
        .map_err(|e| Error::problem("candidate_source_invalid", e.to_string()))?;
    url.path_segments_mut()
        .map_err(|()| Error::problem("candidate_source_invalid", "Invalid repository"))?
        .pop_if_empty()
        .extend(path);
    // Anonymous reads intentionally cannot resolve private sources through ambient credentials.
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| Error::problem("candidate_source_resolution_failed", e.to_string()))?;
    if !response.status().is_success() {
        return Err(Error::problem(
            "candidate_source_resolution_failed",
            format!("GitHub returned {}", response.status()),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::problem("candidate_source_resolution_failed", e.to_string()))?
    {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(Error::problem(
                "candidate_source_resolution_failed",
                "GitHub response exceeded 1 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::problem("candidate_source_resolution_failed", e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    #[tokio::test]
    async fn candidate_pr_resolves_public_fork_head_and_rejects_private_or_foreign_sources() {
        let source = CandidateInput::PullRequest {
            url: "https://github.com/cashubtc/cdk/pull/123".into(),
        };
        let public = json!({"base":{"repo":{"full_name":"cashubtc/cdk","private":false}},"head":{"sha":"B".repeat(40),"repo":{"full_name":"contributor/cdk","private":false}}});
        let resolved = resolve_with(&source, "cashubtc/cdk", |repository, path| {
            assert_eq!(repository, "cashubtc/cdk");
            assert_eq!(path, ["pulls", "123"]);
            std::future::ready(Ok(public.clone()))
        })
        .await
        .unwrap();
        assert_eq!(
            resolved,
            (
                "https://github.com/contributor/cdk.git".into(),
                "b".repeat(40)
            )
        );
        for (path, value) in [
            ("/base/repo/full_name", json!("another/repo")),
            ("/head/repo/private", json!(true)),
            ("/head/repo/full_name", json!("../cdk")),
            ("/head/sha", json!("short")),
        ] {
            let mut invalid = public.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            assert!(
                resolve_with(&source, "cashubtc/cdk", |_, _| std::future::ready(Ok(
                    invalid.clone()
                )))
                .await
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn candidate_fork_commits_require_a_public_fork_of_the_canonical_repository() {
        let commit = "c".repeat(40);
        let source = CandidateInput::Commit {
            sha: Some(commit.clone()),
            url: None,
            repository: Some("contributor/cdk".into()),
        };
        let fork = json!({"full_name":"Contributor/cdk","private":false,"fork":true,
            "parent":{"full_name":"cashubtc/cdk"},"source":{"full_name":"cashubtc/cdk"}});
        let object = json!({"sha":commit.to_ascii_uppercase()});
        let resolve = |fork: Value, object: Value| {
            let source = source.clone();
            let paths = Mutex::new(Vec::new());
            async move {
                let result = resolve_with(&source, "cashubtc/cdk", |repository, path| {
                    assert_eq!(repository, "contributor/cdk");
                    let response = if path.is_empty() { &fork } else { &object };
                    paths.lock().unwrap().push(path);
                    std::future::ready(Ok(response.clone()))
                })
                .await;
                (result, paths.into_inner().unwrap())
            }
        };
        let (resolved, paths) = resolve(fork.clone(), object.clone()).await;
        assert_eq!(
            resolved.unwrap(),
            (
                "https://github.com/Contributor/cdk.git".into(),
                commit.clone()
            )
        );
        assert_eq!(
            paths,
            [vec![], vec!["git".to_owned(), "commits".into(), commit]]
        );
        // A fork of a fork still descends from the canonical repository.
        let mut nested = fork.clone();
        nested["parent"]["full_name"] = json!("someone/cdk");
        assert!(resolve(nested, object.clone()).await.0.is_ok());
        for (path, value) in [
            ("/private", json!(true)),
            ("/fork", json!(false)),
            ("/full_name", json!("someone/cdk")),
            ("/full_name", json!("contributor/../cdk")),
            ("/parent/full_name", json!("cashubtc/nutshell")),
        ] {
            let mut invalid = fork.clone();
            *invalid.pointer_mut(path).unwrap() = value;
            if path == "/parent/full_name" {
                invalid["source"]["full_name"] = json!("cashubtc/nutshell");
            }
            let (result, paths) = resolve(invalid, object.clone()).await;
            assert!(result.is_err(), "{path}");
            assert_eq!(paths.len(), 1, "a rejected fork must not read its commit");
        }
        for object in [json!({"sha":"d".repeat(40)}), json!({})] {
            assert!(resolve(fork.clone(), object).await.0.is_err());
        }
    }

    #[tokio::test]
    async fn candidate_fork_check_follows_each_profile_canonical_repository() {
        let repositories = [
            "cdk",
            "cdk-cli-wallet",
            "nutshell",
            "nutshell-wallet",
            "cocod-wallet",
        ]
        .map(|id| {
            proofstorm_core::candidate_build_profile(id)
                .unwrap()
                .repository
        })
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(repositories.len(), 3);
        let commit = "c".repeat(40);
        for canonical in &repositories {
            let fork = format!("contributor/{}", canonical.split_once('/').unwrap().1);
            let source = CandidateInput::Commit {
                sha: Some(commit.clone()),
                url: None,
                repository: Some(fork.clone()),
            };
            let response = json!({"full_name":fork,"private":false,"fork":true,
                "parent":{"full_name":canonical},"source":{"full_name":canonical}});
            for target in &repositories {
                let result = resolve_with(&source, target, |_, path| {
                    std::future::ready(Ok(if path.is_empty() {
                        response.clone()
                    } else {
                        json!({"sha":commit})
                    }))
                })
                .await;
                // A fork is accepted only for the component whose repository it forks.
                assert_eq!(result.is_ok(), target == canonical, "{fork} as {target}");
            }
        }
    }

    #[tokio::test]
    async fn candidate_tags_peel_nested_objects_and_fail_closed_on_missing_or_noncommit_objects() {
        let source = CandidateInput::Tag {
            tag: "release/v1".into(),
        };
        let paths = Mutex::new(Vec::new());
        let responses = Mutex::new(std::collections::VecDeque::from([
            json!({"object":{"type":"tag","sha":"a".repeat(40)}}),
            json!({"object":{"type":"tag","sha":"b".repeat(40)}}),
            json!({"object":{"type":"commit","sha":"c".repeat(40)}}),
        ]));
        let result = resolve_with(&source, "cashubtc/cdk", |repository, path| {
            assert_eq!(repository, "cashubtc/cdk");
            paths.lock().unwrap().push(path);
            std::future::ready(Ok(responses.lock().unwrap().pop_front().unwrap()))
        })
        .await
        .unwrap();
        assert_eq!(result.1, "c".repeat(40));
        assert_eq!(
            paths.lock().unwrap()[0],
            ["git", "ref", "tags", "release/v1"]
        );
        assert_eq!(paths.lock().unwrap().len(), 3);
        for value in [
            json!({}),
            json!({"object":{"type":"tree","sha":"a".repeat(40)}}),
            json!({"object":{"type":"tag","sha":"a".repeat(40)}}),
        ] {
            assert!(
                resolve_with(&source, "cashubtc/cdk", |_, _| std::future::ready(Ok(
                    value.clone()
                )))
                .await
                .is_err()
            );
        }
        assert!(
            resolve_with(&source, "cashubtc/cdk", |_, _| std::future::ready(Err(
                Error::problem("candidate_source_resolution_failed", "GitHub returned 404")
            )))
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn bundle_sources_check_only_public_prerequisites_on_the_canonical_repository() {
        let (first, second, tip) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let source = CandidateInput::Bundle {
            path: None,
            commit: tip.clone(),
            sha256: Some("d".repeat(64)),
            prerequisites: vec![first.clone(), second.clone()],
        };
        let paths = Mutex::new(Vec::new());
        let resolved = resolve_with(&source, "cashubtc/cdk", |repository, path| {
            assert_eq!(repository, "cashubtc/cdk");
            let sha = path[2].clone();
            paths.lock().unwrap().push(path);
            std::future::ready(Ok(json!({ "sha": sha })))
        })
        .await
        .unwrap();
        assert_eq!(
            resolved,
            ("https://github.com/cashubtc/cdk.git".into(), tip.clone())
        );
        let paths = paths.into_inner().unwrap();
        assert_eq!(
            paths,
            [first, second].map(|sha| vec!["git".to_owned(), "commits".into(), sha])
        );
        assert!(
            !paths.concat().contains(&tip),
            "the private tip never reaches GitHub"
        );

        let missing = resolve_with(&source, "cashubtc/cdk", |_, _| {
            std::future::ready(Err(Error::problem(
                "candidate_source_resolution_failed",
                "GitHub returned 404 Not Found",
            )))
        })
        .await
        .unwrap_err();
        assert_eq!(
            missing.details.unwrap()["code"],
            "candidate_bundle_base_missing"
        );
        let limited = resolve_with(&source, "cashubtc/cdk", |_, _| {
            std::future::ready(Err(Error::problem(
                "candidate_source_resolution_failed",
                "GitHub returned 403 Forbidden",
            )))
        })
        .await
        .unwrap_err();
        assert_eq!(
            limited.details.unwrap()["code"],
            "candidate_source_resolution_failed"
        );
        let unread = CandidateInput::Bundle {
            path: Some("fix.bundle".into()),
            commit: tip,
            sha256: None,
            prerequisites: Vec::new(),
        };
        assert!(
            resolve_with(&unread, "cashubtc/cdk", |_, _| async {
                panic!("an unread bundle must not reach GitHub")
            })
            .await
            .is_err()
        );
    }
}
